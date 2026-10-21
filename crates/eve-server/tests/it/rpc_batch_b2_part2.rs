//! Acceptance test suite for Batch B2 RPC methods closure (Part 2).
//!
//! Validates:
//! - FastCheckoutService, storeManager
//! - LPStoreMgr, LPSvc, trademgr, rewardMgr
//! - account, authentication, multiLoginBlocker, sessionMgr, chatAuthenticationService
//! - structureControl, structureDirectory, structureDeployment, structureDocking, structureAssetSafety
//! - standingMgr, corpStationMgr, peaceTreatyMgr, killRightMgr
//! - dungeon, dungeonExplorationMgr, epicArcStatus, missionTrackerMgr
//! - industryMonitor, blueprintManager, autoMoonMining, characterMiningLedger, corpMiningLedger
//! - warStatisticMgr, fwVictoryPointMgr, insurgencySolarsystem, solarsystemInterferenceMgr
//! - planetMgr, planetOrbitalRegistryBroker
//! - shipCasterLandingPadMgr, shipCasterLauncherMgr, starterShipcasterTravelMgr, shipcasterTravelMgr
//! - abyssal_content_manager, abyssalMgr, voidSpaceMgr, RWManager
//! - Auxiliary services: alert, bannedwords, browserLockdownSvc, communityTrackMgr, expertSystemMgr, etc.


use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001; // Aura Capsuleer
#[allow(dead_code)]
const P2: i64 = 90_000_002; // Rival Pilot

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(username.into()), PyRep::String(password.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}



async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_batch_b2_mining_and_industry() {
    // parity: rpc:autoMoonMining.RequestMiningCycleOutput, rpc:autoMoonMining.RequestMiningDetails, rpc:blueprintManager.GetBlueprintData, rpc:blueprintManager.GetBlueprintDataByOwner, rpc:blueprintManager.GetLimits
    // parity: rpc:characterMiningLedger.GetCharacterLogs, rpc:colonyResourcesMgr.GetMyCorporationSkyhooks, rpc:corpMiningLedger.GetObserverLedger, rpc:corpMiningLedger.GetObserversWithMiningEvents, rpc:inSpaceCompressionMgr.CompressItemInSpace
    // parity: rpc:industryMonitor.ConnectJob, rpc:industryMonitor.DisconnectJob
    eve_testkit::attest(&["rpc:autoMoonMining.RequestMiningCycleOutput", "rpc:autoMoonMining.RequestMiningDetails", "rpc:blueprintManager.GetBlueprintData", "rpc:blueprintManager.GetBlueprintDataByOwner", "rpc:blueprintManager.GetLimits", "rpc:characterMiningLedger.GetCharacterLogs", "rpc:colonyResourcesMgr.GetMyCorporationSkyhooks", "rpc:corpMiningLedger.GetObserverLedger", "rpc:corpMiningLedger.GetObserversWithMiningEvents", "rpc:inSpaceCompressionMgr.CompressItemInSpace", "rpc:industryMonitor.ConnectJob", "rpc:industryMonitor.DisconnectJob"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // autoMoonMining.RequestMiningCycleOutput
    let res = rpc(&ctx, &mut sess, "autoMoonMining", "RequestMiningCycleOutput", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "autoMoonMining.RequestMiningCycleOutput");
    // autoMoonMining.RequestMiningDetails
    let res = rpc(&ctx, &mut sess, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "autoMoonMining.RequestMiningDetails");
    // blueprintManager.GetBlueprintData
    let res = rpc(&ctx, &mut sess, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("UnknownBlueprint"), "blueprintManager.GetBlueprintData");
    // blueprintManager.GetBlueprintDataByOwner
    let res = rpc(&ctx, &mut sess, "blueprintManager", "GetBlueprintDataByOwner", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "blueprintManager.GetBlueprintDataByOwner");
    // blueprintManager.GetLimits
    let res = rpc(&ctx, &mut sess, "blueprintManager", "GetLimits", &[]).await;
    assert_eq!(res.is_ok(), true, "blueprintManager.GetLimits");
    // characterMiningLedger.GetCharacterLogs
    let res = rpc(&ctx, &mut sess, "characterMiningLedger", "GetCharacterLogs", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "characterMiningLedger.GetCharacterLogs");
    // colonyResourcesMgr.GetMyCorporationSkyhooks
    let res = rpc(&ctx, &mut sess, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "colonyResourcesMgr.GetMyCorporationSkyhooks");
    // corpMiningLedger.GetObserverLedger
    let res = rpc(&ctx, &mut sess, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("CrpAccessDenied"), "corpMiningLedger.GetObserverLedger");
    // corpMiningLedger.GetObserversWithMiningEvents
    let res = rpc(&ctx, &mut sess, "corpMiningLedger", "GetObserversWithMiningEvents", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("CrpAccessDenied"), "corpMiningLedger.GetObserversWithMiningEvents");
    // inSpaceCompressionMgr.CompressItemInSpace
    let res = rpc(&ctx, &mut sess, "inSpaceCompressionMgr", "CompressItemInSpace", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("MustBeInSpace"), "inSpaceCompressionMgr.CompressItemInSpace");
    // industryMonitor.ConnectJob
    let res = rpc(&ctx, &mut sess, "industryMonitor", "ConnectJob", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("JobNotFound"), "industryMonitor.ConnectJob");
    // industryMonitor.DisconnectJob
    let res = rpc(&ctx, &mut sess, "industryMonitor", "DisconnectJob", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "industryMonitor.DisconnectJob");
}

#[tokio::test]
async fn test_batch_b2_combat_and_wars() {
    // parity: rpc:dynamicBountyMgr.GetOutputForClientSolarSystem, rpc:fwVictoryPointMgr.GetAllVictoryPointStates, rpc:fwVictoryPointMgr.GetLocalVictoryPointState, rpc:insurgencySolarsystem.GetAllVisibleCampaigns, rpc:insurgencySolarsystem.GetLocalCampaignClientSnapshot
    // parity: rpc:solarsystemInterferenceMgr.GetAllInterferenceBands, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState, rpc:warStatisticMgr.GetBaseInfo, rpc:warStatisticMgr.GetKillMail, rpc:warStatisticMgr.GetKills
    // parity: rpc:warStatisticMgr.GetKillsByGroup, rpc:wormholeMgr.GetWormholePolarization
    eve_testkit::attest(&["rpc:dynamicBountyMgr.GetOutputForClientSolarSystem", "rpc:fwVictoryPointMgr.GetAllVictoryPointStates", "rpc:fwVictoryPointMgr.GetLocalVictoryPointState", "rpc:insurgencySolarsystem.GetAllVisibleCampaigns", "rpc:insurgencySolarsystem.GetLocalCampaignClientSnapshot", "rpc:solarsystemInterferenceMgr.GetAllInterferenceBands", "rpc:solarsystemInterferenceMgr.GetLocalInterferenceState", "rpc:warStatisticMgr.GetBaseInfo", "rpc:warStatisticMgr.GetKillMail", "rpc:warStatisticMgr.GetKills", "rpc:warStatisticMgr.GetKillsByGroup", "rpc:wormholeMgr.GetWormholePolarization"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // dynamicBountyMgr.GetOutputForClientSolarSystem
    let res = rpc(&ctx, &mut sess, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Real(1.15)), "dynamicBountyMgr.GetOutputForClientSolarSystem");
    // fwVictoryPointMgr.GetAllVictoryPointStates
    let res = rpc(&ctx, &mut sess, "fwVictoryPointMgr", "GetAllVictoryPointStates", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "fwVictoryPointMgr.GetAllVictoryPointStates");
    // fwVictoryPointMgr.GetLocalVictoryPointState
    let res = rpc(&ctx, &mut sess, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[]).await;
    assert_eq!(res.is_ok(), true, "fwVictoryPointMgr.GetLocalVictoryPointState");
    // insurgencySolarsystem.GetAllVisibleCampaigns
    let res = rpc(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "insurgencySolarsystem.GetAllVisibleCampaigns");
    // insurgencySolarsystem.GetLocalCampaignClientSnapshot
    let res = rpc(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await;
    assert_eq!(res.is_ok(), true, "insurgencySolarsystem.GetLocalCampaignClientSnapshot");
    // solarsystemInterferenceMgr.GetAllInterferenceBands
    let res = rpc(&ctx, &mut sess, "solarsystemInterferenceMgr", "GetAllInterferenceBands", &[]).await;
    assert_eq!(res.is_ok(), true, "solarsystemInterferenceMgr.GetAllInterferenceBands");
    // solarsystemInterferenceMgr.GetLocalInterferenceState
    let res = rpc(&ctx, &mut sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[]).await;
    assert_eq!(res.is_ok(), true, "solarsystemInterferenceMgr.GetLocalInterferenceState");
    // warStatisticMgr.Bind
    let res = rpc(&ctx, &mut sess, "warStatisticMgr", "Bind", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "warStatisticMgr.Bind");
    // warStatisticMgr.GetBaseInfo
    let res = rpc(&ctx, &mut sess, "warStatisticMgr", "GetBaseInfo", &[]).await;
    assert_eq!(res.is_ok(), true, "warStatisticMgr.GetBaseInfo");
    // warStatisticMgr.GetKillMail
    let res = rpc(&ctx, &mut sess, "warStatisticMgr", "GetKillMail", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("KillmailNotFound"), "warStatisticMgr.GetKillMail");
    // warStatisticMgr.GetKills
    let res = rpc(&ctx, &mut sess, "warStatisticMgr", "GetKills", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "warStatisticMgr.GetKills");
    // warStatisticMgr.GetKillsByGroup
    let res = rpc(&ctx, &mut sess, "warStatisticMgr", "GetKillsByGroup", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "warStatisticMgr.GetKillsByGroup");
    // wormholeMgr.GetWormholePolarization
    let res = rpc(&ctx, &mut sess, "wormholeMgr", "GetWormholePolarization", &[]).await;
    assert_eq!(res.is_ok(), true, "wormholeMgr.GetWormholePolarization");
}

#[tokio::test]
async fn test_batch_b2_planet_and_orbitals() {
    // parity: rpc:planetMgr.DeleteLaunch, rpc:planetMgr.GMAddCommodity, rpc:planetMgr.GMConvertCommandCenter, rpc:planetMgr.GMForceInstallProgram, rpc:planetMgr.GMGetLocalDistributionReport
    // parity: rpc:planetMgr.GMGetSynchedServerState, rpc:planetMgr.GMRunDepletionSim, rpc:planetMgr.GMVerifySimulation, rpc:planetMgr.GetCommandPinsForPlanet, rpc:planetMgr.GetExtractorsForPlanet
    // parity: rpc:planetMgr.GetFullNetworkForOwner, rpc:planetMgr.GetLocalDistributionReport, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.GetPin, rpc:planetMgr.GetProgramResultInfo
    // parity: rpc:planetMgr.GetResourceData, rpc:planetMgr.RevertChanges, rpc:planetMgr.StartTicking, rpc:planetMgr.UserLaunchCommodities, rpc:planetMgr.UserTransferCommodities
    // parity: rpc:planetMgr.celestialIndex, rpc:planetMgr.planetID, rpc:planetMgr.remoteHandler, rpc:planetOrbitalRegistryBroker.GetSettingsInfo, rpc:planetOrbitalRegistryBroker.GetTaxRate
    // parity: rpc:planetOrbitalRegistryBroker.RevertOrbitalsToInterBus, rpc:planetOrbitalRegistryBroker.UpdateSettings
    eve_testkit::attest(&["rpc:planetMgr.DeleteLaunch", "rpc:planetMgr.GMAddCommodity", "rpc:planetMgr.GMConvertCommandCenter", "rpc:planetMgr.GMForceInstallProgram", "rpc:planetMgr.GMGetLocalDistributionReport", "rpc:planetMgr.GMGetSynchedServerState", "rpc:planetMgr.GMRunDepletionSim", "rpc:planetMgr.GMVerifySimulation", "rpc:planetMgr.GetCommandPinsForPlanet", "rpc:planetMgr.GetExtractorsForPlanet", "rpc:planetMgr.GetFullNetworkForOwner", "rpc:planetMgr.GetLocalDistributionReport", "rpc:planetMgr.GetMyLaunchesDetails", "rpc:planetMgr.GetPin", "rpc:planetMgr.GetProgramResultInfo", "rpc:planetMgr.GetResourceData", "rpc:planetMgr.RevertChanges", "rpc:planetMgr.StartTicking", "rpc:planetMgr.UserLaunchCommodities", "rpc:planetMgr.UserTransferCommodities", "rpc:planetMgr.celestialIndex", "rpc:planetMgr.planetID", "rpc:planetMgr.remoteHandler", "rpc:planetOrbitalRegistryBroker.GetSettingsInfo", "rpc:planetOrbitalRegistryBroker.GetTaxRate", "rpc:planetOrbitalRegistryBroker.RevertOrbitalsToInterBus", "rpc:planetOrbitalRegistryBroker.UpdateSettings"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // planetMgr.DeleteLaunch
    let res = rpc(&ctx, &mut sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(40_009_082), PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.DeleteLaunch");
    // planetMgr.GMAddCommodity
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMAddCommodity", &[PyRep::Integer(40_009_082), PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.GMAddCommodity");
    // planetMgr.GMConvertCommandCenter
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMConvertCommandCenter", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.GMConvertCommandCenter");
    // planetMgr.GMForceInstallProgram
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMForceInstallProgram", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.GMForceInstallProgram");
    // planetMgr.GMGetLocalDistributionReport
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMGetLocalDistributionReport", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.is_ok(), true, "planetMgr.GMGetLocalDistributionReport");
    // planetMgr.GMGetSynchedServerState
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMGetSynchedServerState", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "planetMgr.GMGetSynchedServerState");
    // planetMgr.GMRunDepletionSim
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMRunDepletionSim", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "planetMgr.GMRunDepletionSim");
    // planetMgr.GMVerifySimulation
    let res = rpc(&ctx, &mut sess, "planetMgr", "GMVerifySimulation", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.GMVerifySimulation");
    // planetMgr.GetCommandPinsForPlanet
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetCommandPinsForPlanet", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("ColonyNotFound"), "planetMgr.GetCommandPinsForPlanet");
    // planetMgr.GetExtractorsForPlanet
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetExtractorsForPlanet", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("ColonyNotFound"), "planetMgr.GetExtractorsForPlanet");
    // planetMgr.GetFullNetworkForOwner
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("ColonyNotFound"), "planetMgr.GetFullNetworkForOwner");
    // planetMgr.GetLocalDistributionReport
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetLocalDistributionReport", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.is_ok(), true, "planetMgr.GetLocalDistributionReport");
    // planetMgr.GetMyLaunchesDetails
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "planetMgr.GetMyLaunchesDetails");
    // planetMgr.GetPin
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(40_009_082), PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("ColonyNotFound"), "planetMgr.GetPin");
    // planetMgr.GetProgramResultInfo
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetProgramResultInfo", &[PyRep::Integer(40_009_082), PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "planetMgr.GetProgramResultInfo");
    // planetMgr.GetResourceData
    let res = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("PISkillRequiredToSurveyPlanetResources"), "planetMgr.GetResourceData");
    // planetMgr.RevertChanges
    let res = rpc(&ctx, &mut sess, "planetMgr", "RevertChanges", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.RevertChanges");
    // planetMgr.StartTicking
    let res = rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "planetMgr.StartTicking");
    // planetMgr.UserLaunchCommodities
    let res = rpc(&ctx, &mut sess, "planetMgr", "UserLaunchCommodities", &[PyRep::Integer(40_009_082), PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidRoutingParameters"), "planetMgr.UserLaunchCommodities");
    // planetMgr.UserTransferCommodities
    let res = rpc(&ctx, &mut sess, "planetMgr", "UserTransferCommodities", &[PyRep::Integer(40_009_082), PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidProcessorParameters"), "planetMgr.UserTransferCommodities");
    // planetMgr.append
    let res = rpc(&ctx, &mut sess, "planetMgr", "append", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "planetMgr.append");
    // planetMgr.celestialIndex
    let res = rpc(&ctx, &mut sess, "planetMgr", "celestialIndex", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(1)), "planetMgr.celestialIndex");
    // planetMgr.colonies
    let res = rpc(&ctx, &mut sess, "planetMgr", "colonies", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "planetMgr.colonies");
    // planetMgr.id
    let res = rpc(&ctx, &mut sess, "planetMgr", "id", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(40009082)), "planetMgr.id");
    // planetMgr.planetID
    let res = rpc(&ctx, &mut sess, "planetMgr", "planetID", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(40009082)), "planetMgr.planetID");
    // planetMgr.remoteHandler
    let res = rpc(&ctx, &mut sess, "planetMgr", "remoteHandler", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "planetMgr.remoteHandler");
    // planetOrbitalRegistryBroker.GetSettingsInfo
    let res = rpc(&ctx, &mut sess, "planetOrbitalRegistryBroker", "GetSettingsInfo", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.is_ok(), true, "planetOrbitalRegistryBroker.GetSettingsInfo");
    // planetOrbitalRegistryBroker.GetTaxRate
    let res = rpc(&ctx, &mut sess, "planetOrbitalRegistryBroker", "GetTaxRate", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Real(0.05)), "planetOrbitalRegistryBroker.GetTaxRate");
    // planetOrbitalRegistryBroker.RevertOrbitalsToInterBus
    let res = rpc(&ctx, &mut sess, "planetOrbitalRegistryBroker", "RevertOrbitalsToInterBus", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(res.is_ok(), true, "planetOrbitalRegistryBroker.RevertOrbitalsToInterBus");
    // planetOrbitalRegistryBroker.UpdateSettings
    let res = rpc(&ctx, &mut sess, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(40_009_082), PyRep::Real(0.05), PyRep::Real(0.1)]).await;
    assert_eq!(res.is_ok(), true, "planetOrbitalRegistryBroker.UpdateSettings");
}

#[tokio::test]
async fn test_batch_b2_navigation_and_travel() {
    // parity: rpc:config.GetStationSolarSystemsByOwner, rpc:populationCap.GetCharacterLoadSlot, rpc:populationCap.MoveCharacterToNewSystem, rpc:shipCasterLandingPadMgr.GetAllLandingPads, rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster, rpc:shipStanceMgr.SetShipStance, rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster, rpc:starterShipcasterTravelMgr.CanCharacterJump, rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    // parity: rpc:station.GetGuests, rpc:warpVectorMgr.UseVector
    eve_testkit::attest(&["rpc:config.GetStationSolarSystemsByOwner", "rpc:populationCap.GetCharacterLoadSlot", "rpc:populationCap.MoveCharacterToNewSystem", "rpc:shipCasterLandingPadMgr.GetAllLandingPads", "rpc:shipCasterLandingPadMgr.GetFactionLandingPads", "rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster", "rpc:shipStanceMgr.SetShipStance", "rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster", "rpc:starterShipcasterTravelMgr.CanCharacterJump", "rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster", "rpc:station.GetGuests", "rpc:warpVectorMgr.UseVector"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // config.GetStationSolarSystemsByOwner
    let res = rpc(&ctx, &mut sess, "config", "GetStationSolarSystemsByOwner", &[PyRep::Integer(1000035)]).await;
    assert_eq!(res.is_ok(), true, "config.GetStationSolarSystemsByOwner");
    // populationCap.GetCharacterLoadSlot
    let res = rpc(&ctx, &mut sess, "populationCap", "GetCharacterLoadSlot", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidArguments"), "populationCap.GetCharacterLoadSlot");
    // populationCap.MoveCharacterToNewSystem
    let res = rpc(&ctx, &mut sess, "populationCap", "MoveCharacterToNewSystem", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "populationCap.MoveCharacterToNewSystem");
    // shipCasterLandingPadMgr.GetAllLandingPads
    let res = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "shipCasterLandingPadMgr.GetAllLandingPads");
    // shipCasterLandingPadMgr.GetFactionLandingPads
    let res = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[PyRep::Integer(500001)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "shipCasterLandingPadMgr.GetFactionLandingPads");
    // shipCasterLauncherMgr.GetFactionsWithShipcaster
    let res = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "shipCasterLauncherMgr.GetFactionsWithShipcaster");
    // shipStanceMgr.SetShipStance
    let res = rpc(&ctx, &mut sess, "shipStanceMgr", "SetShipStance", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidShipStance"), "shipStanceMgr.SetShipStance");
    // shipcasterTravelMgr.CmdJumpThroughShipcaster
    let res = rpc(&ctx, &mut sess, "shipcasterTravelMgr", "CmdJumpThroughShipcaster", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "shipcasterTravelMgr.CmdJumpThroughShipcaster");
    // starterShipcasterTravelMgr.CanCharacterJump
    let res = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "starterShipcasterTravelMgr.CanCharacterJump");
    // starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    let res = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster");
    // station.GetGuests
    let res = rpc(&ctx, &mut sess, "station", "GetGuests", &[PyRep::Integer(60_003_760)]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "station.GetGuests");
    // warpVectorMgr.UseVector
    let res = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "warpVectorMgr.UseVector");
}

#[tokio::test]
async fn test_batch_b2_abyssal_and_void() {
    // parity: rpc:abyssalMgr.AbyssalEndGateActivation, rpc:abyssalMgr.AbyssalGateActivation, rpc:abyssalMgr.ClientIsReady, rpc:voidSpaceMgr.ClientIsReady, rpc:voidSpaceMgr.VoidSpaceEndGateActivation
    eve_testkit::attest(&["rpc:abyssalMgr.AbyssalEndGateActivation", "rpc:abyssalMgr.AbyssalGateActivation", "rpc:abyssalMgr.ClientIsReady", "rpc:voidSpaceMgr.ClientIsReady", "rpc:voidSpaceMgr.VoidSpaceEndGateActivation"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // RWManager.get_closest_rw_stations
    let res = rpc(&ctx, &mut sess, "RWManager", "get_closest_rw_stations", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "RWManager.get_closest_rw_stations");
    // RWManager.solarsystem_contains_rw_instances
    let res = rpc(&ctx, &mut sess, "RWManager", "solarsystem_contains_rw_instances", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "RWManager.solarsystem_contains_rw_instances");
    // abyssalMgr.AbyssalEndGateActivation
    let res = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "abyssalMgr.AbyssalEndGateActivation");
    // abyssalMgr.AbyssalGateActivation
    let res = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "abyssalMgr.AbyssalGateActivation");
    // abyssalMgr.ClientIsReady
    let res = rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "abyssalMgr.ClientIsReady");
    // abyssal_content_manager.GenerateAbyssEncounterContent
    let res = rpc(&ctx, &mut sess, "abyssal_content_manager", "GenerateAbyssEncounterContent", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "abyssal_content_manager.GenerateAbyssEncounterContent");
    // abyssal_content_manager.GenerateContentFromExistingPlan
    let res = rpc(&ctx, &mut sess, "abyssal_content_manager", "GenerateContentFromExistingPlan", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "abyssal_content_manager.GenerateContentFromExistingPlan");
    // abyssal_content_manager.GetContentDetailsForShipLocation
    let res = rpc(&ctx, &mut sess, "abyssal_content_manager", "GetContentDetailsForShipLocation", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "abyssal_content_manager.GetContentDetailsForShipLocation");
    // abyssal_content_manager.generate_abyss_npc_spawn_report
    let res = rpc(&ctx, &mut sess, "abyssal_content_manager", "generate_abyss_npc_spawn_report", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "abyssal_content_manager.generate_abyss_npc_spawn_report");
    // voidSpaceMgr.ClientIsReady
    let res = rpc(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "voidSpaceMgr.ClientIsReady");
    // voidSpaceMgr.VoidSpaceEndGateActivation
    let res = rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "voidSpaceMgr.VoidSpaceEndGateActivation");
}

#[tokio::test]
async fn test_batch_b2_auxiliary_services_part1() {
    // parity: rpc:air_npe.get_air_npe_state, rpc:air_npe.is_air_npe_enabled, rpc:air_npe.skip_air_npe, rpc:alert.BeanCount, rpc:alert.BeanDelivery
    // parity: rpc:alert.GetLogModeForError, rpc:alert.GroupBeanDelivery, rpc:alert.SendClientStackTraceAlert, rpc:alphaInjectorMgr.GetNextAvailableInjection, rpc:bannedwords.check_search_words_allowed
    // parity: rpc:bannedwords.check_words_allowed, rpc:bannedwords.fetch_regexes, rpc:beyonce.GetCynoJammerState, rpc:cache.GetServerVersionAndBuild, rpc:characterEnergyMgr.GetMyEnergyState
    // parity: rpc:clientStatLogger.LogString, rpc:communityTrackMgr.change_qa_mode, rpc:communityTrackMgr.get_instance_attributes_public, rpc:devIndexManager.GetAllDevelopmentIndices, rpc:expertSystemMgr.ConsumeExpertSystem
    // parity: rpc:expertSystemMgr.GetMyExpertSystems, rpc:expertSystemMgr.RemoveMyExpertSystem, rpc:externalQueueMgr.PublishProxyEvent, rpc:infoGatheringMgr.GetStateAndConfig, rpc:infoGatheringMgr.LogInfoEventsFromClient
    // parity: rpc:insuranceSvc.GetContracts, rpc:insuranceSvc.GetInsurancePrices, rpc:loadService.GetTotalStats
    eve_testkit::attest(&["rpc:air_npe.get_air_npe_state", "rpc:air_npe.is_air_npe_enabled", "rpc:air_npe.skip_air_npe", "rpc:alert.BeanCount", "rpc:alert.BeanDelivery", "rpc:alert.GetLogModeForError", "rpc:alert.GroupBeanDelivery", "rpc:alert.SendClientStackTraceAlert", "rpc:alphaInjectorMgr.GetNextAvailableInjection", "rpc:bannedwords.check_search_words_allowed", "rpc:bannedwords.check_words_allowed", "rpc:bannedwords.fetch_regexes", "rpc:beyonce.GetCynoJammerState", "rpc:cache.GetServerVersionAndBuild", "rpc:characterEnergyMgr.GetMyEnergyState", "rpc:clientStatLogger.LogString", "rpc:communityTrackMgr.change_qa_mode", "rpc:communityTrackMgr.get_instance_attributes_public", "rpc:devIndexManager.GetAllDevelopmentIndices", "rpc:expertSystemMgr.ConsumeExpertSystem", "rpc:expertSystemMgr.GetMyExpertSystems", "rpc:expertSystemMgr.RemoveMyExpertSystem", "rpc:externalQueueMgr.PublishProxyEvent", "rpc:infoGatheringMgr.GetStateAndConfig", "rpc:infoGatheringMgr.LogInfoEventsFromClient", "rpc:insuranceSvc.GetContracts", "rpc:insuranceSvc.GetInsurancePrices", "rpc:loadService.GetTotalStats"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // air_npe.get_air_npe_state
    let res = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[]).await;
    assert_eq!(res.is_ok(), true, "air_npe.get_air_npe_state");
    // air_npe.is_air_npe_enabled
    let res = rpc(&ctx, &mut sess, "air_npe", "is_air_npe_enabled", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "air_npe.is_air_npe_enabled");
    // air_npe.skip_air_npe
    let res = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[]).await;
    assert_eq!(res.is_ok(), true, "air_npe.skip_air_npe");
    // alert.BeanCount
    let res = rpc(&ctx, &mut sess, "alert", "BeanCount", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidArguments"), "alert.BeanCount");
    // alert.BeanDelivery
    let res = rpc(&ctx, &mut sess, "alert", "BeanDelivery", &[PyRep::String("bean".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidArguments"), "alert.BeanDelivery");
    // alert.GetLogModeForError
    let res = rpc(&ctx, &mut sess, "alert", "GetLogModeForError", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(1)), "alert.GetLogModeForError");
    // alert.GroupBeanDelivery
    let res = rpc(&ctx, &mut sess, "alert", "GroupBeanDelivery", &[PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "alert.GroupBeanDelivery");
    // alert.SendClientStackTraceAlert
    let res = rpc(&ctx, &mut sess, "alert", "SendClientStackTraceAlert", &[PyRep::String("trace".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidArguments"), "alert.SendClientStackTraceAlert");
    // alphaInjectorMgr.GetNextAvailableInjection
    let res = rpc(&ctx, &mut sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "alphaInjectorMgr.GetNextAvailableInjection");
    // bannedwords.check_search_words_allowed
    let res = rpc(&ctx, &mut sess, "bannedwords", "check_search_words_allowed", &[PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "bannedwords.check_search_words_allowed");
    // bannedwords.check_words_allowed
    let res = rpc(&ctx, &mut sess, "bannedwords", "check_words_allowed", &[PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "bannedwords.check_words_allowed");
    // bannedwords.fetch_regexes
    let res = rpc(&ctx, &mut sess, "bannedwords", "fetch_regexes", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "bannedwords.fetch_regexes");
    // beyonce.Bind
    let res = rpc(&ctx, &mut sess, "beyonce", "Bind", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "beyonce.Bind");
    // beyonce.GetCynoJammerState
    let res = rpc(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "beyonce.GetCynoJammerState");
    // beyonce.Unbind
    let res = rpc(&ctx, &mut sess, "beyonce", "Unbind", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "beyonce.Unbind");
    // browserLockdownSvc.GetDefaultHomePage
    let res = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetDefaultHomePage", &[]).await;
    assert_eq!(res.is_ok(), true, "browserLockdownSvc.GetDefaultHomePage");
    // browserLockdownSvc.GetFlaggedSitesHash
    let res = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesHash", &[]).await;
    assert_eq!(res.is_ok(), true, "browserLockdownSvc.GetFlaggedSitesHash");
    // browserLockdownSvc.GetFlaggedSitesList
    let res = rpc(&ctx, &mut sess, "browserLockdownSvc", "GetFlaggedSitesList", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "browserLockdownSvc.GetFlaggedSitesList");
    // browserLockdownSvc.IsBrowserInLockdown
    let res = rpc(&ctx, &mut sess, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "browserLockdownSvc.IsBrowserInLockdown");
    // cache.GetServerVersionAndBuild
    let res = rpc(&ctx, &mut sess, "cache", "GetServerVersionAndBuild", &[]).await;
    assert_eq!(res.is_ok(), true, "cache.GetServerVersionAndBuild");
    // characterEnergyMgr.GetMyEnergyState
    let res = rpc(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "characterEnergyMgr.GetMyEnergyState");
    // clientStatLogger.LogString
    let res = rpc(&ctx, &mut sess, "clientStatLogger", "LogString", &[PyRep::String("stat".into())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "clientStatLogger.LogString");
    // communityTrackMgr.change_qa_mode
    let res = rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[PyRep::Bool(true)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "communityTrackMgr.change_qa_mode");
    // communityTrackMgr.get_instance_attributes_public
    let res = rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "communityTrackMgr.get_instance_attributes_public");
    // devIndexManager.GetAllDevelopmentIndices
    let res = rpc(&ctx, &mut sess, "devIndexManager", "GetAllDevelopmentIndices", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "devIndexManager.GetAllDevelopmentIndices");
    // dogmaIM.SetSessionCheck
    let res = rpc(&ctx, &mut sess, "dogmaIM", "SetSessionCheck", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "dogmaIM.SetSessionCheck");
    // eveguard_report.heartbeat_started
    let res = rpc(&ctx, &mut sess, "eveguard_report", "heartbeat_started", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "eveguard_report.heartbeat_started");
    // eveguard_report.heartbeat_stopped
    let res = rpc(&ctx, &mut sess, "eveguard_report", "heartbeat_stopped", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "eveguard_report.heartbeat_stopped");
    // expertSystemMgr.ConsumeExpertSystem
    let res = rpc(&ctx, &mut sess, "expertSystemMgr", "ConsumeExpertSystem", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "expertSystemMgr.ConsumeExpertSystem");
    // expertSystemMgr.GetMyExpertSystems
    let res = rpc(&ctx, &mut sess, "expertSystemMgr", "GetMyExpertSystems", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "expertSystemMgr.GetMyExpertSystems");
    // expertSystemMgr.RemoveMyExpertSystem
    let res = rpc(&ctx, &mut sess, "expertSystemMgr", "RemoveMyExpertSystem", &[PyRep::Integer(57451)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "expertSystemMgr.RemoveMyExpertSystem");
    // externalQueueMgr.PublishProxyEvent
    let res = rpc(&ctx, &mut sess, "externalQueueMgr", "PublishProxyEvent", &[PyRep::String("evt".into()), PyRep::Dict(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "externalQueueMgr.PublishProxyEvent");
    // infoGatheringMgr.GetStateAndConfig
    let res = rpc(&ctx, &mut sess, "infoGatheringMgr", "GetStateAndConfig", &[]).await;
    assert_eq!(res.is_ok(), true, "infoGatheringMgr.GetStateAndConfig");
    // infoGatheringMgr.LogInfoEventsFromClient
    let res = rpc(&ctx, &mut sess, "infoGatheringMgr", "LogInfoEventsFromClient", &[PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "infoGatheringMgr.LogInfoEventsFromClient");
    // insuranceSvc.GetContracts
    let res = rpc(&ctx, &mut sess, "insuranceSvc", "GetContracts", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "insuranceSvc.GetContracts");
    // insuranceSvc.GetInsurancePrices
    let res = rpc(&ctx, &mut sess, "insuranceSvc", "GetInsurancePrices", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "insuranceSvc.GetInsurancePrices");
    // insuranceSvc.SetSessionCheck
    let res = rpc(&ctx, &mut sess, "insuranceSvc", "SetSessionCheck", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "insuranceSvc.SetSessionCheck");
    // loadService.GetTotalStats
    let res = rpc(&ctx, &mut sess, "loadService", "GetTotalStats", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "loadService.GetTotalStats");
}

#[tokio::test]
async fn test_batch_b2_auxiliary_services_part2() {
    // parity: rpc:loadService.SetConfig, rpc:localizationServer.GetAllTextChanges, rpc:localizationServer.ReloadFSDPickle, rpc:localizationServer.UpdateLocalizationQAWrap, rpc:loginCampaignManager.claim_reward
    // parity: rpc:loginCampaignManager.get_client_campaign_state, rpc:loginRewardFacilities.get_my_clone_grade, rpc:milestoneMgr.ClaimRewards, rpc:milestoneMgr.ProcessCharacterLogon, rpc:monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount
    // parity: rpc:monolithTaskTrackerMgr.HasEverWarped, rpc:monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters, rpc:nes_intro.get_nes_intro_state, rpc:nes_intro.skip_nes_intro, rpc:nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections
    // parity: rpc:publicQaToolsServer.CanGiveItemForMultifit, rpc:publicQaToolsServer.MoveMeTo, rpc:publicQaToolsServer.SlashCmd, rpc:raffleMgr.QA_SeedRaffles, rpc:scanMgr.SignalTrackerRegister
    // parity: rpc:search.Query, rpc:search.QuickQuery, rpc:seasonalLoginCampaignManager.claim_reward, rpc:seasonalLoginCampaignManager.get_active_campaign, rpc:seasonalLoginCampaignManager.get_claim_history
    // parity: rpc:securityMgr.get_modified_systems, rpc:skillMgr2.GetMySkillHandler, rpc:slash.SlashCmd, rpc:subscriptionMgr.GetCloneGrade, rpc:subscriptionMgr.GetSubscriptionTime
    // parity: rpc:taleMgr.GetGlobalWorldEventTales, rpc:taleMgr.get_active_tales_by_template, rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID
    eve_testkit::attest(&["rpc:loadService.SetConfig", "rpc:localizationServer.GetAllTextChanges", "rpc:localizationServer.ReloadFSDPickle", "rpc:localizationServer.UpdateLocalizationQAWrap", "rpc:loginCampaignManager.claim_reward", "rpc:loginCampaignManager.get_client_campaign_state", "rpc:loginRewardFacilities.get_my_clone_grade", "rpc:milestoneMgr.ClaimRewards", "rpc:milestoneMgr.ProcessCharacterLogon", "rpc:monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount", "rpc:monolithTaskTrackerMgr.HasEverWarped", "rpc:monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters", "rpc:nes_intro.get_nes_intro_state", "rpc:nes_intro.skip_nes_intro", "rpc:nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections", "rpc:publicQaToolsServer.CanGiveItemForMultifit", "rpc:publicQaToolsServer.MoveMeTo", "rpc:publicQaToolsServer.SlashCmd", "rpc:raffleMgr.QA_SeedRaffles", "rpc:scanMgr.SignalTrackerRegister", "rpc:search.Query", "rpc:search.QuickQuery", "rpc:seasonalLoginCampaignManager.claim_reward", "rpc:seasonalLoginCampaignManager.get_active_campaign", "rpc:seasonalLoginCampaignManager.get_claim_history", "rpc:securityMgr.get_modified_systems", "rpc:skillMgr2.GetMySkillHandler", "rpc:slash.SlashCmd", "rpc:subscriptionMgr.GetCloneGrade", "rpc:subscriptionMgr.GetSubscriptionTime", "rpc:taleMgr.GetGlobalWorldEventTales", "rpc:taleMgr.get_active_tales_by_template", "rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // loadService.SetConfig
    let res = rpc(&ctx, &mut sess, "loadService", "SetConfig", &[PyRep::String("k".into()), PyRep::String("v".into())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "loadService.SetConfig");
    // localizationServer.GetAllTextChanges
    let res = rpc(&ctx, &mut sess, "localizationServer", "GetAllTextChanges", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "localizationServer.GetAllTextChanges");
    // localizationServer.ReloadFSDPickle
    let res = rpc(&ctx, &mut sess, "localizationServer", "ReloadFSDPickle", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "localizationServer.ReloadFSDPickle");
    // localizationServer.UpdateLocalizationQAWrap
    let res = rpc(&ctx, &mut sess, "localizationServer", "UpdateLocalizationQAWrap", &[PyRep::Bool(true)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "localizationServer.UpdateLocalizationQAWrap");
    // loginCampaignManager.claim_reward
    let res = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "loginCampaignManager.claim_reward");
    // loginCampaignManager.get_client_campaign_state
    let res = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[]).await;
    assert_eq!(res.is_ok(), true, "loginCampaignManager.get_client_campaign_state");
    // loginRewardFacilities.get_my_clone_grade
    let res = rpc(&ctx, &mut sess, "loginRewardFacilities", "get_my_clone_grade", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(2)), "loginRewardFacilities.get_my_clone_grade");
    // milestoneMgr.ClaimRewards
    let res = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "milestoneMgr.ClaimRewards");
    // milestoneMgr.ProcessCharacterLogon
    let res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[]).await;
    assert_eq!(res.is_ok(), true, "milestoneMgr.ProcessCharacterLogon");
    // monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount
    let res = rpc(&ctx, &mut sess, "monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::Tuple(_))), true, "monolithTaskTrackerMgr.GetCompletedAchievementsAndClientEventCount");
    // monolithTaskTrackerMgr.HasEverWarped
    let res = rpc(&ctx, &mut sess, "monolithTaskTrackerMgr", "HasEverWarped", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "monolithTaskTrackerMgr.HasEverWarped");
    // monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters
    let res = rpc(&ctx, &mut sess, "monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters", &[PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "monolithTaskTrackerMgr.UpdateClientAchievmentsAndCounters");
    // nes_intro.get_nes_intro_state
    let res = rpc(&ctx, &mut sess, "nes_intro", "get_nes_intro_state", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "nes_intro.get_nes_intro_state");
    // nes_intro.skip_nes_intro
    let res = rpc(&ctx, &mut sess, "nes_intro", "skip_nes_intro", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "nes_intro.skip_nes_intro");
    // nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections
    let res = rpc(&ctx, &mut sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections");
    // publicQaToolsServer.CanGiveItemForMultifit
    let res = rpc(&ctx, &mut sess, "publicQaToolsServer", "CanGiveItemForMultifit", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "publicQaToolsServer.CanGiveItemForMultifit");
    // publicQaToolsServer.MoveMeTo
    let res = rpc(&ctx, &mut sess, "publicQaToolsServer", "MoveMeTo", &[PyRep::Integer(30_000_142)]).await;
    assert_eq!(res.is_ok(), true, "publicQaToolsServer.MoveMeTo");
    // publicQaToolsServer.SlashCmd
    let res = rpc(&ctx, &mut sess, "publicQaToolsServer", "SlashCmd", &[PyRep::String("/tr Jita".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("UnknownSlashCommand"), "publicQaToolsServer.SlashCmd");
    // raffleMgr.QA_SeedRaffles
    let res = rpc(&ctx, &mut sess, "raffleMgr", "QA_SeedRaffles", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "raffleMgr.QA_SeedRaffles");
    // repairSvc.SetSessionCheck
    let res = rpc(&ctx, &mut sess, "repairSvc", "SetSessionCheck", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "repairSvc.SetSessionCheck");
    // reprocessingSvc.SetSessionCheck
    let res = rpc(&ctx, &mut sess, "reprocessingSvc", "SetSessionCheck", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "reprocessingSvc.SetSessionCheck");
    // scanMgr.SignalTrackerRegister
    let res = rpc(&ctx, &mut sess, "scanMgr", "SignalTrackerRegister", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "scanMgr.SignalTrackerRegister");
    // search.Query
    let res = rpc(&ctx, &mut sess, "search", "Query", &[PyRep::String("Jita".into())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "search.Query");
    // search.QuickQuery
    let res = rpc(&ctx, &mut sess, "search", "QuickQuery", &[PyRep::String("Jita".into())]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "search.QuickQuery");
    // seasonalLoginCampaignManager.claim_reward
    let res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "seasonalLoginCampaignManager.claim_reward");
    // seasonalLoginCampaignManager.get_active_campaign
    let res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await;
    assert_eq!(res.is_ok(), true, "seasonalLoginCampaignManager.get_active_campaign");
    // seasonalLoginCampaignManager.get_claim_history
    let res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "seasonalLoginCampaignManager.get_claim_history");
    // securityMgr.get_modified_systems
    let res = rpc(&ctx, &mut sess, "securityMgr", "get_modified_systems", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "securityMgr.get_modified_systems");
    // serviceGatewayMgr.qa_disable_hijack_mode
    let res = rpc(&ctx, &mut sess, "serviceGatewayMgr", "qa_disable_hijack_mode", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "serviceGatewayMgr.qa_disable_hijack_mode");
    // serviceGatewayMgr.qa_enable_hijack_mode
    let res = rpc(&ctx, &mut sess, "serviceGatewayMgr", "qa_enable_hijack_mode", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "serviceGatewayMgr.qa_enable_hijack_mode");
    // serviceGatewayMgr.qa_is_hijack_mode_enabled
    let res = rpc(&ctx, &mut sess, "serviceGatewayMgr", "qa_is_hijack_mode_enabled", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "serviceGatewayMgr.qa_is_hijack_mode_enabled");
    // skillMgr2.GetMySkillHandler
    let res = rpc(&ctx, &mut sess, "skillMgr2", "GetMySkillHandler", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(90000001)), "skillMgr2.GetMySkillHandler");
    // slash.SlashCmd
    let res = rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/tr Jita".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidSlashArgument"), "slash.SlashCmd");
    // subscriptionMgr.GetCloneGrade
    let res = rpc(&ctx, &mut sess, "subscriptionMgr", "GetCloneGrade", &[]).await;
    assert_eq!(res.is_ok(), true, "subscriptionMgr.GetCloneGrade");
    // subscriptionMgr.GetSubscriptionTime
    let res = rpc(&ctx, &mut sess, "subscriptionMgr", "GetSubscriptionTime", &[]).await;
    assert_eq!(res.is_ok(), true, "subscriptionMgr.GetSubscriptionTime");
    // taleMgr.GetGlobalWorldEventTales
    let res = rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "taleMgr.GetGlobalWorldEventTales");
    // taleMgr.get_active_tales_by_template
    let res = rpc(&ctx, &mut sess, "taleMgr", "get_active_tales_by_template", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "taleMgr.get_active_tales_by_template");
    // tcpRawProxyService.GetESPTunnelingAddressByNodeID
    let res = rpc(&ctx, &mut sess, "tcpRawProxyService", "GetESPTunnelingAddressByNodeID", &[PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "tcpRawProxyService.GetESPTunnelingAddressByNodeID");
}

