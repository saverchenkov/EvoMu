//! Comprehensive real-RPC integration coverage for error, validation, boundary,
//! permission, location-guard, cooldown, and read-only query paths across 35+ services.
//!
//! Uses only real `ServerContext::dispatch_rpc`, slash commands, and `SessionState`
//! transitions (no direct `with_mut_snapshot` state seeding shortcuts).

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const STATION_DODIXIE: i64 = 60_011_866;
const SYS_JITA: i64 = 30_000_142;
const CORP_CALDARI_NAVY: i64 = 1_000_035;
const CORP_NPC_OTHER: i64 = 1_000_009;
const PLANET_JITA_IV: i64 = 40_009_049;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(
        sess,
        service,
        Some(bound_id),
        method,
        args,
        &HashMap::new(),
        None,
    )
    .await
}

async fn bind_oid(ctx: &ServerContext, sess: &mut SessionState, service: &str, arg: PyRep) -> u64 {
    let rep = rpc(ctx, sess, service, "MachoBindObject", &[arg, PyRep::None])
        .await
        .expect("MachoBindObject");
    let PyRep::Tuple(parts) = rep else {
        panic!("expected MachoBindObject tuple for {service}, got {rep:?}");
    };
    parts[0].as_i64().expect("bound object id") as u64
}

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn setup_gm(ctx: &ServerContext, session_id: u64) -> (SessionState, i64) {
    let gm_cid = 90_000_099;
    ctx.db
        .provision_gm_account("gm_cov", "gm-cov-secret", gm_cid, "GM Coverage");
    let sess = setup_pilot(ctx, session_id, "gm_cov", "gm-cov-secret", gm_cid).await;
    (sess, gm_cid)
}

async fn slash(ctx: &ServerContext, gm_sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, gm_sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash '{cmd}' failed: {e:?}"))
}

async fn create_fresh_alpha_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_name: &str,
) -> (SessionState, i64) {
    ctx.db.provision_player_account(username, password);
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    let doll = PyRep::Dict(vec![(PyRep::String("genderID".into()), PyRep::Integer(1))]);
    let portrait = PyRep::Dict(vec![(PyRep::String("lighting".into()), PyRep::Integer(1))]);
    let cid = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(char_name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            doll,
            portrait,
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll")
    .as_i64()
    .expect("created character id");
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(cid)],
    )
    .await
    .expect("SelectCharacterID fresh pilot");
    (sess, cid)
}

// =============================================================================
// 1. Authentication & Character-Selection Guards Across 25+ Services
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unauthenticated_and_pre_char_select_guards_across_services() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut anon = SessionState::new(1001);
    let mut pre_char = SessionState::new(1002);
    common::login_pilot(&ctx, &mut pre_char, "demo", "demo-secret").await;

    // Services gated by require_auth / require_character on unauthenticated session
    let unauth_cases: &[(&str, &str, &[PyRep])] = &[
        ("stationSvc", "GetSolarSystem", &[]),
        ("stationSvc", "Undock", &[]),
        ("station", "GetGuests", &[]),
        ("corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(1)]),
        ("skillMgr2", "GetSkills", &[]),
        ("jumpCloneSvc", "GetCloneState", &[]),
        ("insuranceSvc", "GetContracts", &[]),
        ("repairSvc", "GetRepairQuotes", &[]),
        ("charMgr", "GetPrivateInfo", &[]),
        ("marketProxy", "GetCharOrders", &[]),
        ("contractProxy", "GetMyCurrentContractList", &[]),
        ("industryManager", "GetJobsByOwner", &[]),
        ("planetMgr", "GetFullNetworkForOwner", &[]),
        ("LPStoreMgr", "GetAvailableOffersFromCorp", &[]),
        ("LPSvc", "GetAllMyCharacterWalletLPBalances", &[]),
        ("bountyProxy", "GetMyKillRights", &[]),
        ("crimewatch", "GetClientStates", &[]),
        ("search", "Query", &[PyRep::String("Jita".into())]),
        ("lookupSvc", "LookupCharacters", &[PyRep::String("Aura".into())]),
        ("bannedwords", "check_words_allowed", &[PyRep::String("hello".into())]),
        ("bookmark", "BookmarkLocation", &[PyRep::String("Spot".into())]),
        ("fleetObjectHandler", "Init", &[]),
        ("fleetMgr", "CreateFleet", &[]),
        ("corpmgr", "GetAssetInventory", &[]),
        ("petitioner", "CreatePetition", &[PyRep::String("Help".into())]),
        ("ProjectDiscovery", "get_player_analysis_kredits", &[]),
        ("seasonManager", "get_max_active_challenges", &[]),
        ("map", "GetBeaconCount", &[]),
        ("ingamereport", "report_other", &[PyRep::Integer(PILOT2)]),
        ("mailMgr", "GetMailHeaders", &[]),
        ("calendarProxy", "GetEventList", &[]),
        ("agentMgr", "GetMyJournalDetails", &[]),
        ("raffleProxy", "AmIBanned", &[]),
        ("pvpFilamentMgr", "LeavePVPQueue", &[]),
        ("tourneyMgr", "CreateMatch", &[]),
        ("facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)]),
        ("fwCharacterEnlistmentMgr", "GetMyEnlistment", &[]),
        ("sovMgr", "GetSovHubFuelAccessGroup", &[]),
    ];

    for &(svc, meth, args) in unauth_cases {
        let err = rpc(&ctx, &mut anon, svc, meth, args)
            .await
            .unwrap_err_or_panic(svc, meth, "unauthenticated");
        assert!(
            matches!(
                err.code.as_str(),
                "AuthRequired" | "Unauthenticated" | "NotAuthenticated" | "Unauthorized" | "CharacterNotSelected" | "AdminAccessDenied"
            ),
            "unexpected unauthenticated error code for {svc}.{meth}: {err:?}"
        );
    }

    // Authenticated user who has NOT selected a character yet -> require_character must fail
    let pre_char_cases: &[(&str, &str, &[PyRep])] = &[
        ("station", "GetGuests", &[]),
        ("corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(1)]),
        ("skillMgr2", "SaveSkillQueue", &[]),
        ("jumpCloneSvc", "InstallCloneInStation", &[]),
        ("insuranceSvc", "GetContracts", &[]),
        ("repairSvc", "GetRepairQuotes", &[]),
        ("charMgr", "SetCharacterDescription", &[PyRep::String("hi".into())]),
        ("marketProxy", "PlaceBuyOrder", &[]),
        ("contractProxy", "CreateContract", &[]),
        ("industryManager", "GetJob", &[PyRep::Integer(1)]),
        ("planetMgr", "GetColony", &[PyRep::Integer(PLANET_JITA_IV)]),
        ("LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(3414)]),
        ("LPSvc", "ExchangeConcordLP", &[PyRep::Integer(CORP_CALDARI_NAVY), PyRep::Integer(100)]),
        ("bountyProxy", "AddToBounty", &[PyRep::Integer(PILOT2), PyRep::Real(100_000.0)]),
        ("crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(PILOT1)]),
        ("search", "QuickQuery", &[PyRep::String("Jita".into())]),
        ("bookmark", "BookmarkLocation", &[PyRep::String("Spot".into())]),
        ("fleetMgr", "CreateFleet", &[]),
        ("petitioner", "CreatePetition", &[PyRep::String("Test".into())]),
        ("ProjectDiscovery", "get_project_id", &[]),
        ("seasonManager", "get_season_data_for_character", &[]),
        ("ingamereport", "report_macro_use", &[PyRep::Integer(PILOT2)]),
        ("raffleProxy", "AmIBanned", &[]),
        ("pvpFilamentMgr", "LeavePVPQueue", &[]),
        ("fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]),
    ];

    for &(svc, meth, args) in pre_char_cases {
        let err = rpc(&ctx, &mut pre_char, svc, meth, args)
            .await
            .unwrap_err_or_panic(svc, meth, "pre-char-select");
        assert!(
            matches!(
                err.code.as_str(),
                "CharacterNotSelected" | "AuthRequired" | "NoCharacter"
            ),
            "unexpected pre-char error code for {svc}.{meth}: {err:?}"
        );
    }

    // Non-GM pilot calling GM/slash/QA surfaces -> InsufficientRole
    let mut player = setup_pilot(&ctx, 1003, "demo", "demo-secret", PILOT1).await;
    let slash_err = rpc(&ctx, &mut player, "slash", "SlashCmd", &[PyRep::String("/giveisk 100".into())])
        .await
        .expect_err("non-GM slash must fail");
    assert_eq!(slash_err.code, "InsufficientRole");
}

trait UnwrapErrExt {
    fn unwrap_err_or_panic(self, svc: &str, meth: &str, phase: &str) -> BridgeRpcError;
}

impl UnwrapErrExt for Result<PyRep, BridgeRpcError> {
    fn unwrap_err_or_panic(self, svc: &str, meth: &str, phase: &str) -> BridgeRpcError {
        match self {
            Ok(v) => panic!("{svc}.{meth} succeeded unexpectedly during {phase}: {v:?}"),
            Err(e) => e,
        }
    }
}

// =============================================================================
// 2. Station, CorpStationMgr, JumpClones, Certificates, SkillMgr2
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_station_clones_certificates_and_skills_edge_paths() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 2001, "demo", "demo-secret", PILOT1).await;
    let (mut fresh_sess, _) =
        create_fresh_alpha_pilot(&ctx, 2002, "alpha_edge", "alpha-pass", "Alpha Edge").await;

    // --- stationSvc & station & corpStationMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "GetSolarSystem", &[PyRep::Integer(60_999_999)])
            .await
            .unwrap_err()
            .code,
        "StationNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "GetSolarSystem", &[PyRep::Integer(30_999_999)])
            .await
            .unwrap_err()
            .code,
        "SolarSystemNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "GetSolarSystem", &[PyRep::Integer(STATION_JITA)])
            .await
            .expect("GetSolarSystem(station)"),
        PyRep::Integer(SYS_JITA)
    );
    let sys_kv = rpc(&ctx, &mut s1, "stationSvc", "GetSolarSystem", &[])
        .await
        .expect("GetSolarSystem()");
    assert_eq!(sys_kv.dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA)));

    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "GetStation", &[PyRep::Integer(60_999_999)])
            .await
            .unwrap_err()
            .code,
        "StationNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "GetStationItemBits", &[PyRep::Integer(60_999_999)])
            .await
            .unwrap_err()
            .code,
        "StationNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidStationService"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(STATION_JITA)])
            .await
            .unwrap_err()
            .code,
        "AlreadyDocked"
    );

    // Undock fresh pilot -> test space-vs-docked guards
    let fresh_station = fresh_sess.station_id.expect("fresh_sess docked at starter station");
    rpc(&ctx, &mut fresh_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock fresh_sess");
    ctx.sync_session(&mut fresh_sess);

    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "station", "GetGuests", &[])
            .await
            .unwrap_err()
            .code,
        "NotDockedInStation"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "stationSvc", "GetStation", &[])
            .await
            .unwrap_err()
            .code,
        "MissingStationId"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "stationSvc", "GetStationItemBits", &[])
            .await
            .unwrap_err()
            .code,
        "MissingStationId"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NotDocked"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "stationSvc", "Undock", &[])
            .await
            .unwrap_err()
            .code,
        "UndockFailed"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "stationSvc", "Dock", &[])
            .await
            .unwrap_err()
            .code,
        "MissingLocationId"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "stationSvc",
            "Dock",
            &[PyRep::Integer(STATION_JITA), PyRep::Integer(999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipId"
    );
    // Immediate re-dock blocked by 10s session-change timer
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "stationSvc", "Dock", &[PyRep::Integer(fresh_station)])
            .await
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    // Jump clones while undocked -> MustBeDocked
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "InstallCloneInStation", &[])
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "GetStationCloneState", &[])
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "GetPriceForClone", &[])
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );

    // Advance virtual clock past 10s session-change timer -> re-dock cleanly
    ctx.advance_secs(12);
    rpc(&ctx, &mut fresh_sess, "stationSvc", "Dock", &[PyRep::Integer(fresh_station)])
        .await
        .expect("re-dock after session timer");
    ctx.sync_session(&mut fresh_sess);

    // --- jumpCloneSvc (docked) ---
    // Fresh Alpha pilot cannot install jump clones -> OmegaCapacityRequired
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "InstallCloneInStation", &[])
            .await
            .unwrap_err()
            .code,
        "OmegaCapacityRequired"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidJumpCloneID"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(888_888)])
            .await
            .unwrap_err()
            .code,
        "JumpCloneNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "DestroyInstalledClone", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidJumpCloneID"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "DestroyInstalledClone", &[PyRep::Integer(888_888)])
            .await
            .unwrap_err()
            .code,
        "CloneNotFound"
    );
    assert!(rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "GetShipCloneState", &[]).await.is_ok());
    let validate_rep = rpc(&ctx, &mut fresh_sess, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect("ValidateInstallJumpClone returns V24 validation list");
    assert!(
        matches!(
            &validate_rep,
            PyRep::List(errs)
                if errs.iter().any(|e| matches!(e, PyRep::String(s) if s == "UI/Medical/Errors/JumpCloneValidationSkillReqNotFulfilled"))
                    && errs.iter().any(|e| matches!(e, PyRep::Tuple(t) if t.first().and_then(|s| s.as_str()) == Some("UI/Medical/Errors/JumpCloneValidationInsufficientFunds")))
        ),
        "unexpected ValidateInstallJumpClone result: {validate_rep:?}"
    );

    // --- skillMgr2 ---
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(25_538), PyRep::Real(500.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFreeSP"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(999_999), PyRep::Integer(1)])])],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    // Non-skill type 34 (Tritanium)
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1)])])],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    // Unmet prerequisite: Capital Ships (20533) requires Advanced Spaceship Command V + Spaceship Command V
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(20533), PyRep::Integer(1)])])],
        )
        .await
        .unwrap_err()
        .code,
        "SkillPrerequisiteNotMet"
    );
    // Already trained skill (3300 level 1 on PILOT1) -> SkillAlreadyTrained
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(1)])])],
        )
        .await
        .unwrap_err()
        .code,
        "SkillAlreadyTrained"
    );
    // ApplyFreeSkillPoints validations
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(0), PyRep::Real(100.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(34), PyRep::Real(100.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Real(-5.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidSkillPointAmount"
    );
    assert!(rpc(&ctx, &mut fresh_sess, "skillMgr2", "GetFreeSkillPointsAppliedToQueue", &[]).await.is_ok());
    // InjectSkillIntoBrain validations
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "InjectSkillIntoBrain", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(34)])
            .await
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    // PurchaseSkills validations
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![])])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "skillMgr2", "PurchaseSkills", &[PyRep::Integer(34)])
            .await
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    // Read-only skillMgr2 queries
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetAllSkills", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetEndOfTraining", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetSkillHistory", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetRespecInfo", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetSkillChangesForISIS", &[]).await.is_ok());

    // --- insuranceSvc & repairSvc ---
    assert_eq!(
        rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(100_000_002)])
            .await
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(34)])
            .await
            .unwrap_err()
            .code,
        "ShipNotFound"
    );
    let rifter_ins = rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(587)])
        .await
        .expect("Rifter insurance price");
    assert!(rifter_ins.as_f64().is_some_and(|v| v > 0.0));
    assert_eq!(
        rpc(&ctx, &mut s1, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(100_000_002)])
            .await
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );
    assert!(rpc(&ctx, &mut s1, "insuranceSvc", "GetContracts", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrices", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(100_000_001)])]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "repairSvc", "RepairAll", &[PyRep::List(vec![])]).await.is_ok());
}

// =============================================================================
// 3. charUnboundMgr, charMgr, standingMgr, lookupSvc, search, bannedwords
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_character_standings_lookup_search_and_bannedwords_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 3001, "demo", "demo-secret", PILOT1).await;

    // --- charUnboundMgr & charMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(PILOT2)])
            .await
            .unwrap_err()
            .code,
        "CharacterOwnershipMismatch"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "charMgr", "GetPublicInfo3", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "charMgr", "GetPrivateInfo", &[PyRep::Integer(PILOT2)])
            .await
            .unwrap_err()
            .code,
        "CharacterOwnershipMismatch"
    );
    let huge_desc = "x".repeat(5_001);
    assert_eq!(
        rpc(&ctx, &mut s1, "charMgr", "SetCharacterDescription", &[PyRep::String(huge_desc)])
            .await
            .unwrap_err()
            .code,
        "DescriptionTooLong"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(-1), PyRep::Real(5.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidContactID"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(50), PyRep::Real(5.0)])
            .await
            .unwrap_err()
            .code,
        "ContactNotFound"
    );
    // RespecCharacter invalid sum (all 30s -> exceeds total attribute pool)
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "charMgr",
            "RespecCharacter",
            &[
                PyRep::Integer(30),
                PyRep::Integer(30),
                PyRep::Integer(30),
                PyRep::Integer(30),
                PyRep::Integer(30),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "RemapFailed"
    );
    assert!(rpc(&ctx, &mut s1, "charMgr", "GetContactList", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "charMgr", "GetOwnerNoteLabels", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "charMgr", "ListStations", &[]).await.is_ok());

    // --- standingMgr ---
    assert!(rpc(&ctx, &mut s1, "standingMgr", "GetCharStandings", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "standingMgr", "GetStandingTransactions", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "standingMgr", "GetCorpStandings", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "standingMgr", "GetNPCNPCStandings", &[]).await.is_ok());

    // --- lookupSvc ---
    let exact_hit = rpc(
        &ctx,
        &mut s1,
        "lookupSvc",
        "LookupCharacters",
        &[PyRep::String("Aura Capsuleer".into()), PyRep::Bool(true)],
    )
    .await
    .expect("LookupCharacters exact");
    let PyRep::List(exact_list) = exact_hit else {
        panic!("expected list from LookupCharacters");
    };
    assert_eq!(exact_list.len(), 1);
    let miss = rpc(
        &ctx,
        &mut s1,
        "lookupSvc",
        "LookupEvePlayerCharacters",
        &[PyRep::String("NoSuchPilotXYZ999".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupEvePlayerCharacters miss");
    assert_eq!(miss, PyRep::List(vec![]));
    assert!(rpc(&ctx, &mut s1, "lookupSvc", "LookupCorporations", &[PyRep::String("Caldari Navy".into())]).await.is_ok());

    // --- search ---
    let q_dict = rpc(
        &ctx,
        &mut s1,
        "search",
        "Query",
        &[
            PyRep::String("Jita".into()),
            PyRep::List(vec![PyRep::Integer(7), PyRep::Integer(9)]),
            PyRep::Integer(1), // exact=1
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("search.Query");
    assert!(matches!(q_dict, PyRep::Dict(_)));
    let qq = rpc(
        &ctx,
        &mut s1,
        "search",
        "QuickQuery",
        &[
            PyRep::String("Caldari".into()),
            PyRep::List(vec![PyRep::Integer(3)]),
            PyRep::Integer(1), // hideNPC=1 -> should exclude NPC corps
        ],
    )
    .await
    .expect("search.QuickQuery hideNPC");
    assert_eq!(qq, PyRep::List(vec![]));

    // --- bannedwords ---
    let regexes = rpc(&ctx, &mut s1, "bannedwords", "fetch_regexes", &[])
        .await
        .expect("fetch_regexes");
    assert!(matches!(regexes, PyRep::List(ref v) if !v.is_empty()));
    assert_eq!(
        rpc(&ctx, &mut s1, "bannedwords", "check_words_allowed", &[PyRep::String("clean name".into())])
            .await
            .expect("check_words_allowed clean"),
        PyRep::None
    );
    let bad_loc = rpc(&ctx, &mut s1, "bannedwords", "check_words_allowed", &[PyRep::String("exploit station".into())])
        .await
        .unwrap_err();
    assert_eq!(bad_loc.code, "UserError");
    assert_eq!(bad_loc.message, "LocationNameInvalidBannedWord");
    let bad_chat = rpc(&ctx, &mut s1, "bannedwords", "check_chat_words_allowed", &[PyRep::String("total scam".into())])
        .await
        .unwrap_err();
    assert_eq!(bad_chat.code, "UserError");
    assert_eq!(bad_chat.message, "ChatContentInvalidBannedWords");
    assert_eq!(
        rpc(&ctx, &mut s1, "bannedwords", "check_search_words_allowed", &[PyRep::String("hack".into())])
            .await
            .expect("check_search_words_allowed"),
        PyRep::Bool(false)
    );
}

// =============================================================================
// 4. marketProxy, contractProxy, LPStoreMgr, LPSvc, raffleProxy
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_market_contracts_lp_and_hypernet_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 4001, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 4002, "other", "other-secret", PILOT2).await;
    let (mut fresh_sess, _) =
        create_fresh_alpha_pilot(&ctx, 4003, "poor_trader", "poor-secret", "Poor Trader").await;
    let (mut gm, _) = setup_gm(&ctx, 4004).await;

    // --- marketProxy ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[PyRep::Integer(STATION_JITA)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    // Negative price
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(34),
                PyRep::Real(-10.0),
                PyRep::Integer(100),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    // 5-significant-figure price violates tick size rule
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(34),
                PyRep::Real(12345.0),
                PyRep::Integer(10),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );
    // StationMismatch
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_DODIXIE),
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "StationMismatch"
    );
    // VisibilitySkillRequired (fresh pilot has Visibility 0, range 40 > 0)
    let fresh_station = fresh_sess.station_id.expect("fresh_sess starter station");
    assert_eq!(
        rpc(
            &ctx,
            &mut fresh_sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(fresh_station),
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(40),
                PyRep::Integer(1),
                PyRep::Integer(30),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "VisibilitySkillRequired"
    );
    // Overflow check
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(34),
                PyRep::Real(1.0e15),
                PyRep::Integer(1_000_000_000),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "MarketOrderOverflow"
    );
    // InsufficientFunds (s1 has 1B ISK, try placing 2B ISK buy order on an item without NPC sell orders)
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(23_773),
                PyRep::Real(2_000_000_000.0),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(30),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    assert!(rpc(&ctx, &mut s1, "marketProxy", "StartupCheck", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.is_ok());

    // --- contractProxy ---
    // Unsupported contract type
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "contractProxy",
            "CreateContract",
            &[PyRep::Integer(99)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidContractType"
    );
    // Unknown itemID -> ContractItemNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // availability Public
                PyRep::Integer(0), // assignee
                PyRep::Integer(7), // days
                PyRep::Integer(1), // startStationID
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(STATION_JITA),
                PyRep::Real(1_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("Missing item".into()),
                PyRep::None,
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(999_888_777), PyRep::Integer(1)])]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ContractItemNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "contractProxy", "GetContract", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "ContractNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "contractProxy", "AcceptContract", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "ContractNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "contractProxy", "PlaceBid", &[PyRep::Integer(999_999), PyRep::Real(-5.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidBidAmount"
    );

    // Give GM 10 Tritanium via slash -> create a private ItemExchange contract to PILOT2
    let item_rep = slash(&ctx, &mut gm, "/giveitem 34 10").await;
    let created_item_id = item_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("created itemID from /giveitem");
    let contract_rep = rpc(
        &ctx,
        &mut gm,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(1), // Private
            PyRep::Integer(PILOT2),
            PyRep::Integer(7),
            PyRep::Integer(0),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(1_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Private Trade".into()),
            PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(created_item_id), PyRep::Integer(10)])]),
        ],
    )
    .await
    .expect("CreateContract");
    let cid = contract_rep
        .as_i64()
        .or_else(|| contract_rep.dict_get("contractID").and_then(|v| v.as_i64()))
        .expect("contractID");
    // Issuer cannot accept own contract -> CannotAcceptOwnContract
    assert_eq!(
        rpc(&ctx, &mut gm, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)])
            .await
            .unwrap_err()
            .code,
        "CannotAcceptOwnContract"
    );
    // Third pilot cannot accept private contract assigned to PILOT2 -> ContractAccessDenied
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)])
            .await
            .unwrap_err()
            .code,
        "ContractAccessDenied"
    );
    assert!(rpc(&ctx, &mut s1, "contractProxy", "GetMyCurrentContractList", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "contractProxy", "GetMyExpiredContractList", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "contractProxy", "GetMyBids", &[]).await.is_ok());

    // --- LPStoreMgr & LPSvc ---
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(3414), PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidQuantity"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(CORP_NPC_OTHER), PyRep::Integer(3414), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidStationCorpForLPStore"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(999_999), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "OfferNotFound"
    );
    // PILOT2 has 0 Caldari Navy LP -> InsufficientLoyaltyPoints
    assert_eq!(
        rpc(&ctx, &mut s2, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(3414), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "InsufficientLoyaltyPoints"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(0), PyRep::Integer(100)])
            .await
            .unwrap_err()
            .code,
        "InvalidTargetCorp"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(1_000_179), PyRep::Integer(100)])
            .await
            .unwrap_err()
            .code,
        "InvalidTargetCorp"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(CORP_CALDARI_NAVY), PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidLPAmount"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(CORP_CALDARI_NAVY), PyRep::Integer(100)])
            .await
            .unwrap_err()
            .code,
        "NoExistingTargetCorpLP"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(CORP_CALDARI_NAVY), PyRep::Integer(100)])
            .await
            .unwrap_err()
            .code,
        "InsufficientLoyaltyPoints"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[])
            .await
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(0), PyRep::Integer(10)])
            .await
            .unwrap_err()
            .code,
        "InvalidTargetCorp"
    );
    assert!(rpc(&ctx, &mut s1, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(CORP_CALDARI_NAVY)]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[]).await.is_ok());

    // --- raffleProxy ---
    assert_eq!(
        rpc(&ctx, &mut s1, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert!(rpc(&ctx, &mut s1, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(34)]).await.is_ok());
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(999_999), PyRep::Integer(8), PyRep::Real(1_000.0), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRaffleDuration"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(999_999), PyRep::Integer(8), PyRep::Real(1_000.0), PyRep::Integer(3600)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "raffleProxy", "AmIBanned", &[])
            .await
            .expect("AmIBanned"),
        PyRep::Bool(false)
    );

    // Undock s1 -> test NotDockedAtStation / MustBeDockedInStation / InvalidRaffleLocation
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[])
        .await
        .expect("undock s1");
    ctx.sync_session(&mut s1);
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(STATION_JITA),
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(30),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotDockedAtStation"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(3414), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(CORP_CALDARI_NAVY), PyRep::Integer(100)])
            .await
            .unwrap_err()
            .code,
        "MustBeDockedInStation"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(999_999), PyRep::Integer(8), PyRep::Real(1_000.0), PyRep::Integer(3600)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRaffleLocation"
    );
}

// =============================================================================
// 5. industryManager, industryMonitor, planetMgr, moonExtractions
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_industry_pi_and_moon_extractions_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 5001, "demo", "demo-secret", PILOT1).await;

    // --- industryManager & industryMonitor ---
    assert_eq!(
        rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "JobNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "industryManager", "GetJobsByOwner", &[PyRep::Integer(PILOT2)])
            .await
            .unwrap_err()
            .code,
        "OwnerAccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "industryManager", "GetJobsByOwner", &[PyRep::Integer(PILOT1)])
            .await
            .expect("own jobs"),
        PyRep::List(vec![])
    );
    assert!(rpc(&ctx, &mut s1, "industryManager", "GetJobCounts", &[]).await.is_ok());

    assert_eq!(
        rpc(&ctx, &mut s1, "industryMonitor", "ConnectJob", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "industryMonitor", "ConnectJob", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "JobNotFound"
    );

    // --- planetMgr (unbound and bound via MachoBindObject) ---
    assert_eq!(
        rpc(&ctx, &mut s1, "planetMgr", "GetColony", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    let planet_oid = bind_oid(&ctx, &mut s1, "planetMgr", PyRep::Integer(40_009_082)).await;
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "GetColony", &[])
            .await
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "GetPin", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "UserAbandonPlanet", &[])
            .await
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );
    assert!(rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "GetCommandPinsForPlanet", &[]).await.is_ok());
    assert!(rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "GetExtractorsForPlanet", &[]).await.is_ok());
    assert!(rpc_bound(&ctx, &mut s1, "planetMgr", planet_oid, "GetCommandCenterLevel", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "planetMgr", "GetFullNetworkForOwner", &[]).await.is_ok());

    // --- moonExtractions ---
    assert_eq!(
        rpc(&ctx, &mut s1, "moonExtractions", "StartNewExtraction", &[])
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "moonExtractions",
            "StartNewExtraction",
            &[PyRep::Integer(1_000_000_000_001), PyRep::Integer(60)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidExtractionDuration"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "moonExtractions",
            "StartNewExtraction",
            &[PyRep::Integer(1_000_000_000_001), PyRep::Integer(518_400)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
}

// =============================================================================
// 6. corpmgr, corpRegistry, allianceRegistry, warRegistry, facWarMgr, fw*, sovMgr
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_corp_alliance_war_fw_and_sov_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 6001, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 6002, "other", "other-secret", PILOT2).await;
    let (mut gm, _) = setup_gm(&ctx, 6003).await;

    // --- corpmgr ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "corpmgr",
            "GetAssetInventory",
            &[PyRep::Integer(CORP_CALDARI_NAVY)],
        )
        .await
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );
    assert!(rpc(&ctx, &mut s1, "corpmgr", "GetCorporationIDForCharacter", &[PyRep::Integer(PILOT1)]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "corpmgr", "GetPublicInfo", &[PyRep::Integer(CORP_CALDARI_NAVY)]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "corpmgr", "GetAggressionSettings", &[PyRep::Integer(CORP_CALDARI_NAVY)]).await.is_ok());

    // --- allianceRegistry & warRegistry from NPC corp ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "CreateAlliance",
            &[PyRep::String("Bad NPC Ally".into()), PyRep::String("BNA".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "allianceRegistry", "GetAlliance", &[PyRep::Integer(99_999_999)])
            .await
            .unwrap_err()
            .code,
        "AllianceNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "allianceRegistry", "DeclareExecutorSupport", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(CORP_CALDARI_NAVY)])
            .await
            .unwrap_err()
            .code,
        "NpcCorporationCannotDeclareWar"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "warsInfoMgr", "GetPublicWarInfo", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "WarNotFound"
    );

    // Advance past 10s login session-change timer, then create two real player corps via corpRegistry.AddCorporation
    ctx.advance_secs(12);
    let corp1_id = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Edge Alpha Corp".into()), PyRep::String("EAC1".into())],
    )
    .await
    .expect("AddCorporation 1")
    .dict_get("corporationID")
    .and_then(|v| v.as_i64())
    .expect("corp1 id");
    ctx.sync_session(&mut s1);

    let corp2_id = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Edge Beta Corp".into()), PyRep::String("EBC2".into())],
    )
    .await
    .expect("AddCorporation 2")
    .dict_get("corporationID")
    .and_then(|v| v.as_i64())
    .expect("corp2 id");
    ctx.sync_session(&mut s2);

    // MoveCorpHQHere without office -> NoOfficeAtStation
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "MoveCorpHQHere", &[PyRep::Integer(STATION_JITA)])
            .await
            .unwrap_err()
            .code,
        "NoOfficeAtStation"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "allianceRegistry", "DeclareExecutorSupport", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );
    // CreateAlliance without 1B in corp master wallet -> InsufficientFunds
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "CreateAlliance",
            &[PyRep::String("Edge Alliance".into()), PyRep::String("EDGA".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    // DeclareWarAgainst without War HQ -> WarHqRequired, against self -> InvalidWarTarget
    assert_eq!(
        rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp1_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidWarTarget"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp2_id)])
            .await
            .unwrap_err()
            .code,
        "WarHqRequired"
    );

    // --- facWarMgr, fwCharacterEnlistmentMgr, fwWarzoneSolarsystem, fwVictoryPointMgr ---
    // Non-CEO / NPC corp member (GM is in NPC corp) -> AccessDenied
    assert_eq!(
        rpc(&ctx, &mut gm, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    // Invalid militia faction for corp -> InvalidFWMilitiaFaction
    assert_eq!(
        rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "InvalidFWMilitiaFaction"
    );
    // Invalid militia faction for direct character enlistment -> InvalidWarFaction
    assert_eq!(
        rpc(&ctx, &mut s2, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "InvalidWarFaction"
    );
    // Direct enlist s2 -> ok, second enlist -> FwAlreadyEnlisted
    rpc(&ctx, &mut s2, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
        .await
        .expect("JoinFactionAsCharacter");
    ctx.sync_session(&mut s2);
    assert_eq!(
        rpc(&ctx, &mut s2, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "FwAlreadyEnlisted"
    );
    // Leave direct enlistment -> 24h cooldown blocks immediate re-enlistment!
    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[])
        .await
        .expect("RemoveMyDirectEnlistment");
    ctx.sync_session(&mut s2);
    assert_eq!(
        rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
            .await
            .unwrap_err()
            .code,
        "FwEnlistmentCooldown"
    );
    assert!(rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[]).await.is_ok());
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(500_001)])],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // --- sovMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "sovMgr", "SetSovHubFuelAccessGroup", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "sovMgr", "GetSovHubFuelAccessGroup", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert!(rpc(&ctx, &mut s1, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[]).await.is_ok());
}

// =============================================================================
// 7. bountyProxy, aggressionMgr, crimewatch, fleetMgr, fleetObjectHandler,
//    bookmark, beyonce, jumpTimers, dogmaIM, invbroker
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bounty_crimewatch_fleet_bookmarks_and_space_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 7001, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 7002, "other", "other-secret", PILOT2).await;
    let (mut fresh_sess, _) =
        create_fresh_alpha_pilot(&ctx, 7003, "fleet_mate", "fleet-secret", "Fleet Mate").await;

    // --- bountyProxy, aggressionMgr, crimewatch ---
    assert_eq!(
        rpc(&ctx, &mut s1, "bountyProxy", "AddToBounty", &[PyRep::Integer(PILOT2), PyRep::Real(50_000.0)])
            .await
            .unwrap_err()
            .code,
        "BountyAmountTooLow"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "bountyProxy", "AddToBounty", &[PyRep::Integer(777_777_777), PyRep::Real(100_000.0)])
            .await
            .unwrap_err()
            .code,
        "TargetNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut fresh_sess, "bountyProxy", "AddToBounty", &[PyRep::Integer(PILOT2), PyRep::Real(100_000_000.0)])
            .await
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "bountyProxy", "SellKillRight", &[PyRep::Integer(999_999), PyRep::Real(-1.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidPrice"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "bountyProxy", "SellKillRight", &[PyRep::Integer(999_999), PyRep::Real(1_000.0)])
            .await
            .unwrap_err()
            .code,
        "KillRightNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "bountyProxy", "CancelSellKillRight", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "KillRightNotFound"
    );
    assert!(rpc(&ctx, &mut s1, "bountyProxy", "GetBounty", &[PyRep::Integer(PILOT2)]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "bountyProxy", "GetTopPilotBounties", &[PyRep::Integer(1)]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "crimewatch", "GetClientStates", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(PILOT1)]).await.is_ok());

    // --- fleetMgr & fleetObjectHandler ---
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "Init", &[])
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateWing", &[])
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    let fleet_kv = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[PyRep::String("Edge Fleet".into())])
        .await
        .expect("CreateFleet");
    let wing1_id = fleet_kv.dict_get("wingID").and_then(|v| v.as_i64()).expect("wingID");
    let squad1_id = fleet_kv.dict_get("squadID").and_then(|v| v.as_i64()).expect("squadID");
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[])
            .await
            .unwrap_err()
            .code,
        "AlreadyInFleet"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteWing", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "WingNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "KickMember", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "MemberNotFound"
    );

    // Invite PILOT2 -> duplicate invite -> InviteAlreadyPending, then AcceptInvite, test permission denied
    rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(PILOT2), PyRep::Integer(wing1_id), PyRep::Integer(squad1_id), PyRep::Integer(4)],
    )
    .await
    .expect("Invite PILOT2");
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "fleetObjectHandler",
            "Invite",
            &[PyRep::Integer(PILOT2), PyRep::Integer(wing1_id), PyRep::Integer(squad1_id), PyRep::Integer(4)],
        )
        .await
        .unwrap_err()
        .code,
        "InviteAlreadyPending"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[])
            .await
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );
    ctx.advance_secs(12);
    rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[])
        .await
        .expect("PILOT2 AcceptInvite");
    ctx.sync_session(&mut s2);
    // wing1_id now contains PILOT2 -> DeleteWing must reject with WingNotEmpty
    assert_eq!(
        rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(wing1_id)])
            .await
            .unwrap_err()
            .code,
        "WingNotEmpty"
    );
    // Squad member PILOT2 cannot CreateWing (-> FleetPermissionDenied) or KickMember on boss (-> CannotKickBoss)
    assert_eq!(
        rpc(&ctx, &mut s2, "fleetObjectHandler", "CreateWing", &[])
            .await
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "fleetObjectHandler", "KickMember", &[PyRep::Integer(PILOT1)])
            .await
            .unwrap_err()
            .code,
        "CannotKickBoss"
    );

    // --- bookmark ---
    assert_eq!(
        rpc(&ctx, &mut s1, "bookmark", "BookmarkLocation", &[PyRep::String("   ".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidBookmarkLabel"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("a".repeat(101))],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBookmarkLabel"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::Integer(STATION_JITA), PyRep::Integer(999_999), PyRep::String("Test".into())],
        )
        .await
        .unwrap_err()
        .code,
        "FolderNotFound"
    );
    let bm_kv = rpc(
        &ctx,
        &mut s1,
        "bookmark",
        "BookmarkLocation",
        &[PyRep::Integer(STATION_JITA), PyRep::Integer(0), PyRep::String("Jita BM".into())],
    )
    .await
    .expect("BookmarkLocation Jita");
    let bm_id = bm_kv.dict_get("bookmarkID").and_then(|v| v.as_i64()).expect("bookmarkID");
    // Multi-session permission check: PILOT2 cannot delete PILOT1's bookmark
    assert_eq!(
        rpc(&ctx, &mut s2, "bookmark", "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // --- beyonce, jumpTimers, dogmaIM, invbroker ---
    let bey_oid = bind_oid(&ctx, &mut s1, "beyonce", PyRep::Integer(SYS_JITA)).await;
    // Docked in station -> NotInSpace
    assert_eq!(
        rpc_bound(
            &ctx,
            &mut s1,
            "beyonce",
            bey_oid,
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        )
        .await
        .unwrap_err()
        .code,
        "NotInSpace"
    );
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "beyonce", bey_oid, "CmdSetSpeedFraction", &[PyRep::Real(0.5)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert!(rpc(&ctx, &mut s1, "jumpTimers", "GetTimers", &[]).await.is_ok());

    // Undock s1 -> test invalid space navigation args
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[])
        .await
        .expect("undock s1 for beyonce checks");
    ctx.sync_session(&mut s1);
    let bey_oid2 = bind_oid(&ctx, &mut s1, "beyonce", PyRep::Integer(SYS_JITA)).await;
    assert_eq!(
        rpc_bound(
            &ctx,
            &mut s1,
            "beyonce",
            bey_oid2,
            "CmdGotoDirection",
            &[PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "beyonce", bey_oid2, "CmdSetSpeedFraction", &[PyRep::Real(-0.5)])
            .await
            .unwrap_err()
            .code,
        "InvalidSpeedFraction"
    );
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "beyonce", bey_oid2, "CmdOrbit", &[PyRep::Integer(0), PyRep::Real(500.0)])
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        rpc_bound(
            &ctx,
            &mut s1,
            "beyonce",
            bey_oid2,
            "CmdFollowBall",
            &[PyRep::Integer(999_999_999), PyRep::Real(500.0)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotFound"
    );

    // dogmaIM & invbroker errors
    assert_eq!(
        rpc(&ctx, &mut s1, "dogmaIM", "InjectSkillIntoBrain", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let ship_id = s1.ship_id.expect("active ship");
    let inv_oid = bind_oid(
        &ctx,
        &mut s1,
        "invbroker",
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(5)]),
    )
    .await;
    assert_eq!(
        rpc_bound(&ctx, &mut s1, "invbroker", inv_oid, "Add", &[PyRep::Integer(999_888_777)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
}

// =============================================================================
// 8. agentMgr, mailMgr, calendarMgr, calendarProxy, petitioner,
//    ProjectDiscovery, seasonManager, map, ingamereport, pvpFilamentMgr, tourneyMgr
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_agent_mail_calendar_petition_discovery_season_map_report_and_tourney_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 8001, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 8002, "other", "other-secret", PILOT2).await;
    let (mut gm, _) = setup_gm(&ctx, 8003).await;

    // --- agentMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "agentMgr", "GetSolarSystemOfAgent", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "UnknownAgent"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_020_034)],
        )
        .await
        .unwrap_err()
        .code,
        "AgentNotCoLocated"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "agentMgr", "GetMissionBriefingInfo", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "MissionNotActive"
    );
    assert!(rpc(&ctx, &mut s1, "agentMgr", "GetAgents", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "agentMgr", "GetDungeonShipRestrictions", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "agentMgr", "GetEntryPoint", &[]).await.is_ok());

    // --- mailMgr ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(PILOT2), PyRep::String("Local".into())],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "mailMgr",
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(0), PyRep::String("Local".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidTargetCharacter"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "mailMgr",
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(PILOT2), PyRep::String("   ".into())],
        )
        .await
        .unwrap_err()
        .code,
        "PokeMessageRequired"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "mailMgr",
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(999_999_999), PyRep::String("Local".into())],
        )
        .await
        .unwrap_err()
        .code,
        "TargetCharacterNotFound"
    );
    assert!(rpc(&ctx, &mut s1, "mailMgr", "GetMailHeaders", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "mailMgr", "SyncMail", &[]).await.is_ok());

    // --- calendarMgr & calendarProxy ---
    assert_eq!(
        rpc(&ctx, &mut s1, "calendarProxy", "GetEventDetails", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "CalendarEventNotFound"
    );
    let pers_ev = rpc(
        &ctx,
        &mut s1,
        "calendarMgr",
        "CreatePersonalEvent",
        &[
            PyRep::Integer(135_700_000_000_000_000),
            PyRep::Integer(60),
            PyRep::String("Private Op".into()),
            PyRep::String("Details".into()),
            PyRep::Integer(0),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("CreatePersonalEvent");
    let ev_id = pers_ev.dict_get("eventID").and_then(|v| v.as_i64()).expect("eventID");
    // Uninvited PILOT2 cannot view or delete PILOT1's personal event
    assert_eq!(
        rpc(&ctx, &mut s2, "calendarProxy", "GetEventDetails", &[PyRep::Integer(ev_id)])
            .await
            .unwrap_err()
            .code,
        "CalendarAccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "calendarMgr", "DeleteEvent", &[PyRep::Integer(ev_id)])
            .await
            .unwrap_err()
            .code,
        "CalendarAccessDenied"
    );
    assert!(rpc(&ctx, &mut s1, "calendarProxy", "GetEventList", &[]).await.is_ok());

    // --- petitioner ---
    assert_eq!(
        rpc(&ctx, &mut s1, "petitioner", "CreatePetition", &[PyRep::String("   ".into())])
            .await
            .unwrap_err()
            .code,
        "InvalidPetitionSubject"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "petitioner",
            "CreatePetition",
            &[PyRep::String("Stuck".into()), PyRep::String("Help".into()), PyRep::Integer(9999)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPetitionCategory"
    );
    let pet_rep = rpc(
        &ctx,
        &mut s1,
        "petitioner",
        "CreatePetition",
        &[PyRep::String("Need assistance".into()), PyRep::String("Details".into()), PyRep::Integer(1)],
    )
    .await
    .expect("CreatePetition");
    let pet_id = pet_rep
        .as_i64()
        .or_else(|| pet_rep.dict_get("petitionID").and_then(|v| v.as_i64()))
        .expect("petitionID");
    // Non-owner PILOT2 cannot view PILOT1's petition messages
    let pet_denied = rpc(&ctx, &mut s2, "petitioner", "GetPetitionMessages", &[PyRep::Integer(pet_id)])
        .await
        .expect_err("PILOT2 cannot view PILOT1 petition");
    assert!(
        matches!(pet_denied.code.as_str(), "PetitionAccessDenied" | "AccessDenied" | "GMRoleRequired"),
        "unexpected petition access error: {pet_denied:?}"
    );

    // --- ProjectDiscovery ---
    let pd_err1 = rpc(&ctx, &mut s1, "ProjectDiscovery", "post_classification", &[PyRep::Integer(1)])
        .await
        .unwrap_err();
    assert_eq!(pd_err1.code, "InvalidClassification"); // rpc-lint: allow error code assertion
    let pd_err2 = rpc(
        &ctx,
        &mut s1,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("not_a_task".into()), PyRep::Dict(vec![(PyRep::String("x".into()), PyRep::Integer(1))])],
    )
    .await
    .unwrap_err();
    assert_eq!(pd_err2.code, "InvalidTaskId"); // rpc-lint: allow error code assertion
    assert!(rpc(&ctx, &mut s1, "ProjectDiscovery", "is_enabled", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut s1, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.is_ok());

    // --- seasonManager ---
    let season_err1 = rpc(&ctx, &mut s1, "seasonManager", "select_season", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(season_err1.code, "InvalidSeasonId"); // rpc-lint: allow error code assertion
    let season_err2 = rpc(&ctx, &mut s1, "seasonManager", "get_challenge", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(season_err2.code, "ChallengeNotFound"); // rpc-lint: allow error code assertion
    let season_err3 = rpc(&ctx, &mut s1, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(season_err3.code, "ChallengeNotFound"); // rpc-lint: allow error code assertion
    assert!(rpc(&ctx, &mut s1, "seasonManager", "get_max_active_challenges", &[]).await.is_ok());

    // --- map ---
    assert!(rpc(&ctx, &mut s1, "map", "GetBeaconCount", &[]).await.is_ok());

    // --- ingamereport ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "ingamereport",
            "report_other",
            &[PyRep::Integer(PILOT2), PyRep::String("Suspicious behavior".into())],
        )
        .await
        .expect("ingamereport.report_other"),
        PyRep::None
    );

    // --- pvpFilamentMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "pvpFilamentMgr", "LeavePVPQueue", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NotInPvpQueue"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(999_888_777)])
            .await
            .unwrap_err()
            .code,
        "MustBeInSpace"
    );
    assert!(rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetAllEvents", &[]).await.is_ok());

    // --- tourneyMgr ---
    assert_eq!(
        rpc(&ctx, &mut s1, "tourneyMgr", "CreateMatch", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "AdminAccessDenied"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "MatchNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "tourneyMgr", "GetTeamMembers", &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "MatchNotFound"
    );
}

#[tokio::test]
async fn test_gh829_state_navy_cerberus_archetype_faction_and_npc_corp_id() {
    use eve_server::combat::npc_ai::NpcSpawnManager;
    use eve_server::services::combat_crimewatch::{
        combat_npc_ai::{derive_npc_archetype, spawn_specialized_npc_entity},
        with_combat_runtime,
    };
    use eve_server::services::wormhole_pve_seeding::spawn_sleeper_sentry_tower;
    use glam::DVec3;

    let ctx = ServerContext::new();
    let sys_id = SYS_JITA as u32;

    // 1. Secondary root cause (#829): State Navy Cerberus (52936, group 4034, SDE faction 500027)
    // and Republic Fleet Muninn (52959) must classify as "Navy", while real EDENCOM hulls (54335)
    // still classify as "EDENCOM".
    assert_eq!(derive_npc_archetype(&ctx, 52_936), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 52_959), "Navy");
    assert_eq!(derive_npc_archetype(&ctx, 54_335), "EDENCOM");

    // 2. Spawn State Navy Cerberus with Caldari Navy context (spawn_group_id = 500_001)
    let navy_npc_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        52_936,
        "CaldariNavy",
        500_001,
        DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;

    let (npc_faction_id, npc_archetype) = with_combat_runtime(&ctx, |rt| {
        let npc = rt
            .npc_entities
            .get(&navy_npc_id)
            .expect("State Navy Cerberus present in CombatRuntime.npc_entities");
        (npc.faction_id, npc.archetype.clone())
    });
    assert_eq!(npc_faction_id, 500_001, "Caldari Navy Cerberus must get Caldari faction 500001, never EDENCOM 500027");
    assert_eq!(npc_archetype, "CaldariNavy");

    // 3. Also spawn via NpcSpawnManager::spawn_group, GM /spawn 52936, and spawn_sleeper_sentry_tower
    let (group_id, group_faction_id) = ctx
        .sde
        .npc_spawn_groups
        .values()
        .find(|g| g.faction_id >= 500_001 && !g.npc_type_ids.is_empty())
        .map(|g| (g.spawn_group_id, g.faction_id))
        .expect("SDE has at least one faction npc_spawn_group");
    let mut spawn_mgr = NpcSpawnManager::new();
    let group_npc_ids = spawn_mgr
        .spawn_group(&ctx, sys_id, group_id, DVec3::new(25_000.0, 0.0, 0.0))
        .await
        .expect("NpcSpawnManager::spawn_group succeeds");
    assert!(!group_npc_ids.is_empty());

    let (mut gm_sess, _gm_cid) = setup_gm(&ctx, 82_901).await;
    let gm_spawn_rep = slash(&ctx, &mut gm_sess, "/spawn 52936").await;
    let gm_npc_id = gm_spawn_rep
        .dict_get("spawnedID")
        .and_then(|v| v.as_i64())
        .expect("/spawn 52936 returns spawnedID");

    let wh_sys = 31_000_001u32;
    let tower_id = spawn_sleeper_sentry_tower(&ctx, wh_sys, 3, DVec3::new(35_000.0, 0.0, 0.0)).await;

    // 4. Primary root cause (#829): SlimItemMeta.corp_id must never be a faction ID (500001..=500030)
    // and SlimItem / AddBalls2 wire encoding for State Navy Cerberus must emit Caldari owner & valid corpID.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        let navy_ball = bp.balls.get(&navy_npc_id).expect("Navy Cerberus ball in ballpark");
        let navy_meta = bp
            .ball_slim_meta
            .get(&navy_npc_id)
            .expect("Navy Cerberus SlimItemMeta in ballpark");
        assert_eq!(navy_meta.owner_id, 500_001);
        assert!(
            !(500_001..=500_030).contains(&navy_meta.corp_id),
            "corp_id must never be a faction ID (got {})",
            navy_meta.corp_id
        );
        assert_eq!(
            ctx.sde
                .npc_corporations
                .get(&navy_meta.corp_id)
                .and_then(|c| c.faction_id),
            Some(500_001),
            "State Navy Cerberus corpID {} must belong to Caldari (500001)",
            navy_meta.corp_id
        );

        let slim_py = eve_destiny::wire::slim_item_dict(navy_ball, navy_meta);
        assert_eq!(slim_py.dict_get("ownerID"), Some(&PyRep::Integer(500_001)));
        assert_eq!(
            slim_py.dict_get("corpID"),
            Some(&PyRep::Integer(navy_meta.corp_id))
        );

        let add_balls2 = eve_destiny::wire::encode_add_balls2_action(1, &[navy_ball], Some(&bp));
        let wire_slim = match &add_balls2 {
            PyRep::Tuple(outer) => match &outer[1] {
                PyRep::Tuple(act) => match &act[1] {
                    PyRep::Tuple(wrap) => match &wrap[0] {
                        PyRep::Tuple(inner) => match &inner[1] {
                            PyRep::List(entries) => match &entries[0] {
                                PyRep::Tuple(pair) => pair[0].clone(),
                                other => panic!("unexpected entry shape: {other:?}"),
                            },
                            other => panic!("unexpected extra_data: {other:?}"),
                        },
                        other => panic!("unexpected wrap[0]: {other:?}"),
                    },
                    other => panic!("unexpected act[1]: {other:?}"),
                },
                other => panic!("unexpected outer[1]: {other:?}"),
            },
            other => panic!("unexpected AddBalls2: {other:?}"),
        };
        assert_eq!(wire_slim.dict_get("ownerID"), Some(&PyRep::Integer(500_001)));
        assert_eq!(
            wire_slim.dict_get("corpID"),
            Some(&PyRep::Integer(navy_meta.corp_id))
        );

        // Fallback corp_id = -1 must encode on the wire as PyRep::None (not -1 / faction ID)
        let mut fallback_meta = navy_meta.clone();
        fallback_meta.corp_id = -1;
        let fallback_slim = eve_destiny::wire::slim_item_dict(navy_ball, &fallback_meta);
        assert_eq!(fallback_slim.dict_get("corpID"), Some(&PyRep::None));

        let group_meta = bp
            .ball_slim_meta
            .get(&group_npc_ids[0])
            .expect("spawn_group NPC SlimItemMeta");
        assert!(
            !(500_001..=500_030).contains(&group_meta.corp_id),
            "spawn_group NPC (faction {group_faction_id}) leaked faction ID into corp_id={}",
            group_meta.corp_id
        );
        if group_meta.corp_id > 0 {
            assert!(
                ctx.sde.npc_corporations.contains_key(&group_meta.corp_id),
                "spawn_group corp_id {} missing from sde.npc_corporations",
                group_meta.corp_id
            );
        }

        let gm_meta = bp
            .ball_slim_meta
            .get(&gm_npc_id)
            .expect("/spawn 52936 SlimItemMeta");
        assert!(
            !(500_001..=500_030).contains(&gm_meta.corp_id),
            "/spawn 52936 leaked faction ID into corp_id={}",
            gm_meta.corp_id
        );
        assert!(
            ctx.sde.npc_corporations.contains_key(&gm_meta.corp_id),
            "/spawn 52936 corp_id {} missing from sde.npc_corporations",
            gm_meta.corp_id
        );
    }

    let wh_handle = ctx.systems.get_or_spawn(wh_sys);
    let wh_bp = wh_handle.ballpark.read().unwrap();
    let tower_meta = wh_bp
        .ball_slim_meta
        .get(&tower_id)
        .expect("Sleeper sentry tower SlimItemMeta");
    assert!(
        !(500_001..=500_030).contains(&tower_meta.corp_id),
        "Sleeper sentry tower leaked faction ID into corp_id={}",
        tower_meta.corp_id
    );
    assert!(
        tower_meta.corp_id == -1 || ctx.sde.npc_corporations.contains_key(&tower_meta.corp_id),
        "Sleeper sentry tower corp_id {} invalid",
        tower_meta.corp_id
    );
}
