use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::*;

fn dummy_sync_handler(
    _ctx: &ServerContext,
    _session: &mut SessionState,
    _service: &str,
    _method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Ok(PyRep::None))
}

#[test]
fn default_compiled_registry_builds_without_duplicates_and_looks_up_entries() {
    let registry = RpcRegistry::default_compiled();
    let entries = registry.all_entries();
    assert!(
        entries.len() >= 190,
        "expected >= 190 Track 2A compiled entries, got {}",
        entries.len()
    );

    let energy = registry
        .lookup("characterEnergyMgr", "GetMyEnergyState")
        .expect("characterEnergyMgr.GetMyEnergyState");
    assert_eq!(energy.who, RpcCallerClass::Character);
    assert_eq!(energy.scene, RpcSceneClass::Either);
    assert_eq!(energy.access, RpcAccessKind::Read);

    let lookup = registry
        .lookup("lookupSvc", "LookupCharacters")
        .expect("lookupSvc.LookupCharacters");
    assert_eq!(lookup.who, RpcCallerClass::Account);

    let overload = registry
        .lookup("dogmaIM", "Overload")
        .expect("dogmaIM.Overload");
    assert_eq!(overload.scene, RpcSceneClass::InSpace);
    assert_eq!(overload.access, RpcAccessKind::Write);

    let tourney = registry
        .lookup("tourneyMgr", "TeleportToArena")
        .expect("tourneyMgr.TeleportToArena");
    assert_eq!(tourney.who, RpcCallerClass::Character);

    let reload_season = registry
        .lookup("seasonManager", "reload_fsd_data")
        .expect("seasonManager.reload_fsd_data");
    assert_eq!(reload_season.who, RpcCallerClass::Gm);

    let fix_pd = registry
        .lookup("ProjectDiscovery", "fix_task_id")
        .expect("ProjectDiscovery.fix_task_id");
    assert_eq!(fix_pd.who, RpcCallerClass::Gm);

    assert!(registry.lookup("characterEnergyMgr", "NoSuchMethod").is_none()); // rpc-lint: allow negative lookup test fixture
    assert!(registry.lookup("nonexistentService", "Foo").is_none()); // rpc-lint: allow negative lookup test fixture
}

#[test]
fn duplicate_service_method_returns_err() {
    static SLICE_A: &[RpcEntry] = &[rpc_entry(
        "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
        "Echo",
        RpcCallerClass::Character,
        RpcSceneClass::Either,
        RpcAccessKind::Read,
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];
    static SLICE_B: &[RpcEntry] = &[rpc_entry(
        "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
        "Echo",
        RpcCallerClass::Character,
        RpcSceneClass::Either,
        RpcAccessKind::Write,
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];

    let err = RpcRegistry::try_from_slices(&[SLICE_A, SLICE_B])
        .expect_err("duplicate (service, method) must fail validation");
    assert!(
        err.contains("testSvc.Echo"),
        "error must identify duplicate pair, got: {err}"
    );
}

#[test]
#[should_panic(expected = "duplicate compiled RPC registration: testSvc.DupMethod")]
fn duplicate_service_method_panics_on_from_slices() {
    static SLICE: &[RpcEntry] = &[
        rpc_entry(
            "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
            "DupMethod",
            RpcCallerClass::Character,
            RpcSceneClass::Either,
            RpcAccessKind::Read,
            RpcDispatchHandler::Sync(dummy_sync_handler),
        ),
        rpc_entry(
            "testSvc", // rpc-lint: allow synthetic unit test RPC fixture
            "DupMethod",
            RpcCallerClass::Character,
            RpcSceneClass::Either,
            RpcAccessKind::Write,
            RpcDispatchHandler::Sync(dummy_sync_handler),
        ),
    ];

    let _ = RpcRegistry::from_slices(&[SLICE]);
}

#[test]
fn empty_service_or_method_rejected() {
    static EMPTY_SVC: &[RpcEntry] = &[rpc_entry(
        "",
        "Foo",
        RpcCallerClass::Character,
        RpcSceneClass::Either,
        RpcAccessKind::Read,
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];
    static EMPTY_METHOD: &[RpcEntry] = &[rpc_entry(
        "testSvc",
        "",
        RpcCallerClass::Character,
        RpcSceneClass::Either,
        RpcAccessKind::Read,
        RpcDispatchHandler::Sync(dummy_sync_handler),
    )];

    assert!(RpcRegistry::try_from_slices(&[EMPTY_SVC]).is_err());
    assert!(RpcRegistry::try_from_slices(&[EMPTY_METHOD]).is_err());
}

#[test]
fn compiled_rpc_module_files_match_disk_and_have_non_empty_entries() {
    let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut seen_files = BTreeSet::new();
    for module in COMPILED_RPC_MODULES {
        assert!(
            seen_files.insert(module.file),
            "duplicate module path in COMPILED_RPC_MODULES: {}",
            module.file
        );
        assert!(
            !module.rpcs.is_empty(),
            "migrated module {} must export at least one RPCS entry",
            module.file
        );
        let full = src_root.join(module.file);
        assert!(
            full.is_file(),
            "compiled module file missing on disk: {}",
            full.display()
        );
    }
    assert_eq!(compiled_rpc_pairs().len(), RpcRegistry::default_compiled().all_entries().len());
}

#[tokio::test]
async fn server_context_routes_compiled_rpcs_and_rejects_unregistered_migrated_methods() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    assert!(ctx
        .rpc_registry
        .lookup("characterEnergyMgr", "GetMyEnergyState")
        .is_some());

    let mut sess = SessionState::new(98_101);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // SyncNoKwargs path: characterEnergyMgr.GetMyEnergyState
    let energy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "characterEnergyMgr",
            None,
            "GetMyEnergyState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterEnergyMgr.GetMyEnergyState succeeds");
    assert!(!matches!(energy_rep, PyRep::None));

    // SyncNoService path: ingamereport.report_other
    let report_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ingamereport",
            None,
            "report_other",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("ingamereport.report_other succeeds");
    assert_eq!(report_rep, PyRep::None);

    // SyncReadOnlySession path: lookupSvc.LookupCharacters
    let lookup_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCharacters",
            &[PyRep::String("Demo".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("lookupSvc.LookupCharacters succeeds");
    assert!(matches!(lookup_rep, PyRep::List(_)));

    // Async path via submodule tourney_arena: tourneyMgr.GetPotentialMatches
    let tourney_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "tourneyMgr",
            None,
            "GetPotentialMatches",
            &[],
            &kw,
            None,
        )
        .await
        .expect("tourneyMgr.GetPotentialMatches succeeds");
    assert!(matches!(tourney_rep, PyRep::List(_)));

    // Unregistered methods on migrated services must fail with UnimplementedRPC
    // and never fall through to removed handlers.
    let unregistered_cases = [
        ("characterEnergyMgr", "UnregisteredEnergyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("expertSystemMgr", "UnregisteredExpertMethod"), // rpc-lint: allow negative dispatch test fixture
        ("node_graph", "unregistered_graph_method"), // rpc-lint: allow negative dispatch test fixture
        ("certificateMgr", "UnregisteredCertMethod"), // rpc-lint: allow negative dispatch test fixture
        ("lookupSvc", "UnregisteredLookupMethod"), // rpc-lint: allow negative dispatch test fixture
        ("bannedwords", "unregistered_banned_method"), // rpc-lint: allow negative dispatch test fixture
        ("search", "UnregisteredSearchMethod"), // rpc-lint: allow negative dispatch test fixture
        ("ingamereport", "unregistered_report"), // rpc-lint: allow negative dispatch test fixture
        ("FastCheckoutService", "UnregisteredCheckoutMethod"), // rpc-lint: allow negative dispatch test fixture
        ("activityMgr", "UnregisteredActivityMethod"), // rpc-lint: allow negative dispatch test fixture
        ("LPStoreMgr", "UnregisteredLpMethod"), // rpc-lint: allow negative dispatch test fixture
        ("LPSvc", "UnregisteredLpSvcMethod"), // rpc-lint: allow negative dispatch test fixture
        ("rewardMgr", "UnregisteredRewardMethod"), // rpc-lint: allow negative dispatch test fixture
        ("RWManager", "unregistered_rw_method"), // rpc-lint: allow negative dispatch test fixture
        ("operationsManager", "unregistered_operation"), // rpc-lint: allow negative dispatch test fixture
        ("survey", "UnregisteredSurveyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("eventLog", "UnregisteredTelemetryMethod"), // rpc-lint: allow negative dispatch test fixture
        ("pvpFilamentMgr", "UnregisteredFilamentMethod"), // rpc-lint: allow negative dispatch test fixture
        ("ProjectDiscovery", "unregistered_pd_method"), // rpc-lint: allow negative dispatch test fixture
        ("seasonManager", "unregistered_season_method"), // rpc-lint: allow negative dispatch test fixture
        ("tourneyMgr", "UnregisteredTourneyMethod"), // rpc-lint: allow negative dispatch test fixture
        ("moonExtractions", "UnregisteredMoonMethod"), // rpc-lint: allow negative dispatch test fixture
        ("autoMoonMining", "UnregisteredAutoMoonMethod"), // rpc-lint: allow negative dispatch test fixture
        ("inSpaceCompressionMgr", "UnregisteredCompression"), // rpc-lint: allow negative dispatch test fixture
        ("structureCompressionMgr", "UnregisteredStructureCompression"), // rpc-lint: allow negative dispatch test fixture
        ("corpMiningLedger", "UnregisteredCorpLedger"), // rpc-lint: allow negative dispatch test fixture
        ("characterMiningLedger", "UnregisteredCharLedger"), // rpc-lint: allow negative dispatch test fixture
        ("dogmaIM", "UnregisteredDogmaMethod"), // rpc-lint: allow negative dispatch test fixture
    ];

    for (svc, method) in unregistered_cases {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unregistered method on migrated service must fail");
        assert_eq!(
            err.code, "UnimplementedRPC",
            "expected UnimplementedRPC for {svc}.{method}, got {}",
            err.code
        );
    }

    // Legacy method on `operationsManager` still served by unmigrated `client_login`
    // must continue to dispatch until Track 2B/2C migrates `client_login`.
    let tutorial_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "operationsManager",
            None,
            "can_character_play_the_tutorial",
            &[],
            &kw,
            None,
        )
        .await;
    assert!(
        tutorial_ok.is_ok(),
        "operationsManager.can_character_play_the_tutorial must remain callable via client_login fallback: {tutorial_ok:?}"
    );
}
