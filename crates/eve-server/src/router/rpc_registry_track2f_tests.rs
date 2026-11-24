use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use eve_machonet::SessionState;
use eve_proto::PyRep;

use super::*;

#[test]
fn track_2f_registry_registers_all_modules_without_duplicates() {
    let files: HashSet<&str> = COMPILED_RPC_MODULES.iter().map(|m| m.file).collect();
    for (expected_file, expected_len) in [
        ("services/account_services.rs", 32),
        ("services/petitioner_ops.rs", 31),
        ("services/auth_cache.rs", 14),
        ("services/space_entity_ops.rs", 21),
        ("services/mining_sites.rs", 3),
        ("services/space_warp_jump.rs", 6),
        ("services/space_probe_scanning.rs", 10),
        ("services/pve_exploration.rs", 14),
        ("services/pve_hacking_sites.rs", 7),
        ("services/pve_dungeon_cache.rs", 50),
        ("services/combat_crimewatch.rs", 1),
        ("services/combat_legacy_ess.rs", 4),
        ("services/crimewatch_timers.rs", 14),
    ] {
        assert!(
            files.contains(expected_file),
            "missing Track 2F module {expected_file} from COMPILED_RPC_MODULES"
        );
        let mod_entry = COMPILED_RPC_MODULES
            .iter()
            .find(|m| m.file == expected_file)
            .expect("module checked above");
        assert_eq!(
            mod_entry.rpcs.len(),
            expected_len,
            "unexpected RPCS count for {expected_file}"
        );
    }

    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert!(COMPILED_RPC_MODULES.len() >= 120);
    assert!(registry.all_entries().len() >= 1924);

    for (svc, method) in [
        ("authentication", "Logout"),
        ("userSvc", "UserLogOffCharacter"),
        ("userSvc", "GetRedeemTokens"),
        ("multiLoginBlocker", "Login"),
        ("subscriptionMgr", "GetSubscriptionTime"),
        ("petitioner", "CreatePetition"),
        ("petitioner", "GetCategories"),
        ("machoNet", "GetServiceInfo"),
        ("machoNet", "GetTime"),
        ("cache", "GetServerVersionAndBuild"),
        ("authentication", "Login"),
        ("subscriptionMgr", "GetCloneGrade"),
        ("loginRewardFacilities", "get_my_clone_grade"),
        ("entity", "CmdUnanchor"),
        ("beyonce", "GetRelativity"),
        ("sovHubSvc", "SaveUpgradeConfiguration"),
        ("sovHubSvc", "GetInstalledUpgradesLocal"),
        ("beyonce", "CmdWarpToStuff"),
        ("beyonce", "CmdDock"),
        ("beyonce", "Undock"),
        ("scanMgr", "GetFullState"),
        ("scanMgr", "RecoverProbes"),
        ("scanSvc", "EnterCombatAnomaly"),
        ("keeper", "EnterNextDungeonRoom"),
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails"),
        ("hackingMgr", "StartNewGameInstance"),
        ("dungeon", "DEGetArchetypes"),
        ("keeper", "GetCurrentDungeonForCharacter"),
        ("custom_agency_provider", "get_content_data"),
        ("dungeonInstanceCacheMgr", "GetCombatAnomalyInstances"),
        ("dynamicBountyMgr", "GetOutputForClientSolarSystem"),
        ("dogmaIM", "FireWeapon"), // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted dogmaIM.FireWeapon
        ("beyonce", "CmdAttackTarget"), // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted beyonce.CmdAttackTarget
        ("essMgr", "AttemptLinkToMainBank"),
        ("killRightMgr", "SellKillRight"),
        ("crimewatch", "GetMySecurityStatus"),
        ("aggressionMgr", "GetCriminalTimeStamps"), // rpc-lint: allow legacy aggressionMgr alias
        ("crimewatch", "ApplyTagsForSecStatus"),
        ("account", "GiveCashFromCorpAccount"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "expected Track 2F entry {svc}.{method} in compiled registry"
        );
    }

    // Track 2G entries are now migrated into COMPILED_RPC_MODULES.
    for (svc, method) in [
        ("slash", "SlashCmd"),
        ("charUnboundMgr", "SelectCharacterID"),
        ("keeper", "ActivateAccelerationGate"),
        ("beyonce", "ActivateAccelerationGate"),
        ("keeper", "Bind"),
        ("scanMgr", "ConeScan"),
        ("scanMgr", "RequestScans"),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances"),
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances"),
        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "expected Track 2G entry {svc}.{method} in compiled registry"
        );
    }

    // Legacy client-module/UI alias spellings must stay out of COMPILED_RPC_MODULES (#1194).
    for (svc, method) in [
        ("crimewatchSvc", "GetMySecurityStatus"),
        ("beyonce", "StargateJump"),
    ] {
        assert!(
            registry.lookup(svc, method).is_none(),
            "expected alias {svc}.{method} to stay outside compiled registry"
        );
    }
}

#[tokio::test]
async fn track_2f_dispatch_routes_new_modules_and_preserves_hooks() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(98_601);
    sess.user_id = Some(1);
    sess.role = ServerContext::ROLE_GM_CANONICAL_MASK;
    let kw = HashMap::new();

    // auth_cache: pre-character RPC + cluster call-counter pre-hook + GetCloneGrade pre-hook
    if let Err(err) = ctx
        .dispatch_rpc(&mut sess, "cache", None, "GetServerVersionAndBuild", &[], &kw, None)
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
    let before_macho = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    if let Err(err) = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
    let after_macho = ctx
        .cluster_state
        .read()
        .expect("cluster_state rwlock")
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    assert_eq!(after_macho, before_macho + 1);
    if let Err(err) = ctx
        .dispatch_rpc(&mut sess, "subscriptionMgr", None, "GetCloneGrade", &[], &kw, None)
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    // account_services
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "userSvc",
            None,
            "GetSessionPlaytimeMinutes",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());

    // Bind character for petitioner / space / PvE / combat Track 2F modules and Track 2G fallbacks
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);

    assert!(ctx
        .dispatch_rpc(&mut sess, "petitioner", None, "GetCategories", &[], &kw, None)
        .await
        .is_ok());
    if let Err(err) = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetRelativity", &[], &kw, None)
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
    assert!(ctx
        .dispatch_rpc(&mut sess, "sovHubSvc", None, "GetInstalledUpgradesLocal", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetFullState", &[], &kw, None)
        .await
        .is_ok());
    if let Err(err) = ctx
        .dispatch_rpc(&mut sess, "dungeon", None, "DEGetArchetypes", &[], &kw, None)
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "dynamicBountyMgr",
            None,
            "GetOutputForClientSolarSystem",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    let legacy_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "FireWeapon", // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted dogmaIM.FireWeapon
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("deleted dogmaIM.FireWeapon must return explicit domain error");
    assert_eq!(legacy_err.code, "UnimplementedRPC"); // rpc-lint: allow Phase 4 exit gate error code assertion
    assert!(
        legacy_err.message.contains("dogmaIM.FireWeapon is deleted"),
        "expected combat_legacy_ess handler message, got: {}",
        legacy_err.message
    );
    for (svc, method) in [
        ("beyonce", "CmdDock"),
        ("keeper", "GetDungeonShipRestrictions"),
        ("hackingMgr", "StartNewGameInstance"),
    ] {
        if let Err(err) = ctx.dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None).await {
            assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
        }
    }
    assert!(ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "GetMySecurityStatus", &[], &kw, None)
        .await
        .is_ok());

    // scanMgr.ConeScan must stay routed to space_cyno_filaments (returning PyRep::List, not fleet_social Rowset)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });
    let mut scan_sess = SessionState::new(98_602);
    scan_sess.user_id = Some(1);
    scan_sess.character_id = Some(90_000_001);
    scan_sess.solar_system_id = Some(30_000_142);
    scan_sess.ship_id = Some(100_000_001);
    let cone_scan_rep = ctx
        .dispatch_rpc(
            &mut scan_sess,
            "scanMgr",
            None,
            "ConeScan",
            &[PyRep::Real(std::f64::consts::TAU), PyRep::Real(100_000.0)],
            &kw,
            None,
        )
        .await
        .expect("scanMgr.ConeScan must stay routed through space_cyno_filaments");
    assert!(
        matches!(cone_scan_rep, PyRep::List(_)),
        "scanMgr.ConeScan must return PyRep::List, got {cone_scan_rep:?}"
    );

    // Track 2G slash.SlashCmd 3-way dispatch preserved cleanly
    if let Err(err) = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
    {
        assert_ne!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    // Unregistered method on a Track 2F service rejects cleanly
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "petitioner",
            None,
            "UnregisteredPetitionerMethod", // rpc-lint: allow negative dispatch test fixture
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("unregistered migrated Track 2F method must fail with UnimplementedRPC");
    assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
}

#[tokio::test]
async fn give_cash_from_corp_account_dispatches_both_wallet_ledger_and_division_transfers() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let ceo_id = 90_000_001_i64;
    let target_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
        }
    });
    let mut sess = SessionState::new(98_603);
    sess.user_id = Some(1);
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);

    // (a) wallet_ledger corp-to-character payout (with reason kwarg)
    let dividend_kw = HashMap::from([(
        "reason".to_string(),
        PyRep::String("Corp dividend".into()),
    )]);
    let payout_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(target_id),
                PyRep::Real(50_000.0),
                PyRep::Integer(1000),
            ],
            &dividend_kw,
            None,
        )
        .await
        .expect("wallet_ledger payout via GiveCashFromCorpAccount succeeds");
    assert_eq!(payout_rep, PyRep::None);

    // (b) fleet_social_corps 3-arg corp division-to-division transfer (div 1 -> div 2)
    let empty_kw = HashMap::new();
    let div_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(100.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("fleet_social_corps division transfer via GiveCashFromCorpAccount succeeds");
    assert_eq!(
        div_rep.dict_get("fromDivisionID").and_then(PyRep::as_i64),
        Some(1)
    );
    assert_eq!(
        div_rep.dict_get("toDivisionID").and_then(PyRep::as_i64),
        Some(2)
    );
    assert_eq!(
        div_rep.dict_get("transferredISK").and_then(PyRep::as_f64),
        Some(100.0)
    );

    // (c) invalid 3-arg division transfer still hits handle_corps error (never None / InternalError)
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(1),
                PyRep::Integer(target_id),
                PyRep::Real(100.0),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect_err("invalid 3-arg division transfer must return InvalidTransferDivision");
    assert_eq!(err.code, "InvalidTransferDivision");

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_id];
        assert_eq!(
            corp.division_balance(1),
            eve_db::IskCents::from_isk(99_949_900)
        );
        assert_eq!(
            corp.division_balance(2),
            eve_db::IskCents::from_isk(100)
        );
        assert_eq!(
            snap.characters[&target_id].balance,
            eve_db::IskCents::from_isk(50_050_000)
        );
    });
}
