//! Acceptance test suite for Batch B2 RPC methods closure (Part 1).
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
async fn test_batch_b2_fast_checkout_and_store() {
    // parity: rpc:FastCheckoutService.BuyOffer, rpc:FastCheckoutService.ClearFastCheckoutCacheForUser, rpc:FastCheckoutService.GetOffersForUser, rpc:FastCheckoutService.GetTestingConfiguration, rpc:FastCheckoutService.IsFastCheckoutEnabledForUser
    // parity: rpc:storeManager.buy_offer, rpc:storeManager.can_purchase_offer, rpc:storeManager.get_categories, rpc:storeManager.get_offers, rpc:storeManager.get_products
    eve_testkit::attest(&["rpc:FastCheckoutService.BuyOffer", "rpc:FastCheckoutService.ClearFastCheckoutCacheForUser", "rpc:FastCheckoutService.GetOffersForUser", "rpc:FastCheckoutService.GetTestingConfiguration", "rpc:FastCheckoutService.IsFastCheckoutEnabledForUser", "rpc:storeManager.buy_offer", "rpc:storeManager.can_purchase_offer", "rpc:storeManager.get_categories", "rpc:storeManager.get_offers", "rpc:storeManager.get_products"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // FastCheckoutService.BuyOffer
    let res = rpc(&ctx, &mut sess, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("FastCheckoutDisabled"), "FastCheckoutService.BuyOffer");
    // FastCheckoutService.ClearFastCheckoutCacheForUser
    let res = rpc(&ctx, &mut sess, "FastCheckoutService", "ClearFastCheckoutCacheForUser", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "FastCheckoutService.ClearFastCheckoutCacheForUser");
    // FastCheckoutService.GetOffersForUser
    let res = rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "FastCheckoutService.GetOffersForUser");
    // FastCheckoutService.GetTestingConfiguration
    let res = rpc(&ctx, &mut sess, "FastCheckoutService", "GetTestingConfiguration", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::Dict(_))), true, "FastCheckoutService.GetTestingConfiguration");
    // FastCheckoutService.IsFastCheckoutEnabledForUser
    let res = rpc(&ctx, &mut sess, "FastCheckoutService", "IsFastCheckoutEnabledForUser", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "FastCheckoutService.IsFastCheckoutEnabledForUser");
    // storeManager.buy_offer
    let res = rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("OfferNotFound"), "storeManager.buy_offer");
    // storeManager.can_purchase_offer
    let res = rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("OfferNotFound"), "storeManager.can_purchase_offer");
    // storeManager.get_categories
    let res = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "storeManager.get_categories");
    // storeManager.get_offers
    let res = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "storeManager.get_offers");
    // storeManager.get_products
    let res = rpc(&ctx, &mut sess, "storeManager", "get_products", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "storeManager.get_products");
}

#[tokio::test]
async fn test_batch_b2_lp_and_rewards() {
    // parity: rpc:LPStoreMgr.GetAvailableOffersFromCorp, rpc:LPStoreMgr.TakeOfferForCharacter, rpc:LPStoreMgr.TakeOfferForCorporation, rpc:LPSvc.ExchangeConcordLP, rpc:LPSvc.GetAllMyCharacterWalletLPBalances
    // parity: rpc:LPSvc.GetAllMyCorporationWalletLPBalances, rpc:LPSvc.TransferLPFromMyCorpWalletToOtherCorp, rpc:LPSvc.TransferLPFromMyWalletToOtherCorp, rpc:rewardMgr.GetDelayedRewardsByGroupIDs, rpc:rewardMgr.GetRewardLPLogs
    // parity: rpc:trademgr.InitiateTrade
    eve_testkit::attest(&["rpc:LPStoreMgr.GetAvailableOffersFromCorp", "rpc:LPStoreMgr.TakeOfferForCharacter", "rpc:LPStoreMgr.TakeOfferForCorporation", "rpc:LPSvc.ExchangeConcordLP", "rpc:LPSvc.GetAllMyCharacterWalletLPBalances", "rpc:LPSvc.GetAllMyCorporationWalletLPBalances", "rpc:LPSvc.TransferLPFromMyCorpWalletToOtherCorp", "rpc:LPSvc.TransferLPFromMyWalletToOtherCorp", "rpc:rewardMgr.GetDelayedRewardsByGroupIDs", "rpc:rewardMgr.GetRewardLPLogs", "rpc:trademgr.InitiateTrade"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // LPStoreMgr.GetAvailableOffersFromCorp
    let res = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_035)]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "LPStoreMgr.GetAvailableOffersFromCorp");
    // LPStoreMgr.TakeOfferForCharacter
    let res = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1001), PyRep::Integer(1)]).await;
    assert_eq!(res.is_ok(), true, "LPStoreMgr.TakeOfferForCharacter");
    // LPStoreMgr.TakeOfferForCorporation
    let res = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(1001), PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InsufficientLoyaltyPoints"), "LPStoreMgr.TakeOfferForCorporation");
    // LPSvc.ExchangeConcordLP
    let res = rpc(&ctx, &mut sess, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(1_000_035), PyRep::Integer(100)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InsufficientLoyaltyPoints"), "LPSvc.ExchangeConcordLP");
    // LPSvc.GetAllMyCharacterWalletLPBalances
    let res = rpc(&ctx, &mut sess, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "LPSvc.GetAllMyCharacterWalletLPBalances");
    // LPSvc.GetAllMyCorporationWalletLPBalances
    let res = rpc(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "LPSvc.GetAllMyCorporationWalletLPBalances");
    // LPSvc.TransferLPFromMyCorpWalletToOtherCorp
    let res = rpc(&ctx, &mut sess, "LPSvc", "TransferLPFromMyCorpWalletToOtherCorp", &[PyRep::Integer(1_000_044), PyRep::Integer(100)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidLPAmount"), "LPSvc.TransferLPFromMyCorpWalletToOtherCorp");
    // LPSvc.TransferLPFromMyWalletToOtherCorp
    let res = rpc(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(1_000_035), PyRep::Integer(1_000_044), PyRep::Integer(100)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InsufficientLoyaltyPoints"), "LPSvc.TransferLPFromMyWalletToOtherCorp");
    // rewardMgr.GetDelayedRewardsByGroupIDs
    let res = rpc(&ctx, &mut sess, "rewardMgr", "GetDelayedRewardsByGroupIDs", &[PyRep::List(vec![PyRep::Integer(1)])]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Dict(vec![])), "rewardMgr.GetDelayedRewardsByGroupIDs");
    // rewardMgr.GetRewardLPLogs
    let res = rpc(&ctx, &mut sess, "rewardMgr", "GetRewardLPLogs", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "rewardMgr.GetRewardLPLogs");
    // trademgr.InitiateTrade
    let res = rpc(&ctx, &mut sess, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await;
    assert_eq!(res.is_ok(), true, "trademgr.InitiateTrade");
}

#[tokio::test]
async fn test_batch_b2_account_and_auth() {
    // parity: rpc:account.GetCashBalance, rpc:account.GetDefaultContactCost, rpc:account.GetWalletDivisionsInfo, rpc:account.GiveCash, rpc:account.SetContactCost
    // parity: rpc:authentication.AccruedTime, rpc:authentication.AmUnderage, rpc:authentication.Login, rpc:authentication.Logout, rpc:authentication.SetLanguageID
    // parity: rpc:chatAuthenticationService.GetAuthenticationToken, rpc:multiLoginBlocker.Login, rpc:sessionMgr.RemoveSessionsFromServer
    eve_testkit::attest(&["rpc:account.GetCashBalance", "rpc:account.GetDefaultContactCost", "rpc:account.GetWalletDivisionsInfo", "rpc:account.GiveCash", "rpc:account.SetContactCost", "rpc:authentication.AccruedTime", "rpc:authentication.AmUnderage", "rpc:authentication.Login", "rpc:authentication.Logout", "rpc:authentication.SetLanguageID", "rpc:chatAuthenticationService.GetAuthenticationToken", "rpc:multiLoginBlocker.Login", "rpc:sessionMgr.RemoveSessionsFromServer"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // account.GetCashBalance
    let res = rpc(&ctx, &mut sess, "account", "GetCashBalance", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Real(1000000000.0)), "account.GetCashBalance");
    // account.GetDefaultContactCost
    let res = rpc(&ctx, &mut sess, "account", "GetDefaultContactCost", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "account.GetDefaultContactCost");
    // account.GetWalletDivisionsInfo
    let res = rpc(&ctx, &mut sess, "account", "GetWalletDivisionsInfo", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("CorporationNotFound"), "account.GetWalletDivisionsInfo");
    // account.GiveCash
    let res = rpc(&ctx, &mut sess, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(100.0)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Real(999999900.0)), "account.GiveCash");
    // account.SetContactCost
    let res = rpc(&ctx, &mut sess, "account", "SetContactCost", &[PyRep::Real(50.0)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "account.SetContactCost");
    // authentication.AccruedTime
    let res = rpc(&ctx, &mut sess, "authentication", "AccruedTime", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Integer(0)), "authentication.AccruedTime");
    // authentication.AmUnderage
    let res = rpc(&ctx, &mut sess, "authentication", "AmUnderage", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "authentication.AmUnderage");
    // authentication.Login
    let res = rpc(&ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await;
    assert_eq!(res.is_ok(), true, "authentication.Login");
    // authentication.Logout
    let res = rpc(&ctx, &mut sess, "authentication", "Logout", &[]).await;
    assert_eq!(res.is_ok(), true, "authentication.Logout");
    // authentication.SetLanguageID
    let res = rpc(&ctx, &mut sess, "authentication", "SetLanguageID", &[PyRep::String("EN".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("Unauthorized"), "authentication.SetLanguageID");
    // chatAuthenticationService.GetAuthenticationToken
    let res = rpc(&ctx, &mut sess, "chatAuthenticationService", "GetAuthenticationToken", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("Unauthorized"), "chatAuthenticationService.GetAuthenticationToken");
    // multiLoginBlocker.Login
    let res = rpc(&ctx, &mut sess, "multiLoginBlocker", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("Unauthorized"), "multiLoginBlocker.Login");
    // sessionMgr.RemoveSessionsFromServer
    let res = rpc(&ctx, &mut sess, "sessionMgr", "RemoveSessionsFromServer", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "sessionMgr.RemoveSessionsFromServer");
}

#[tokio::test]
async fn test_batch_b2_structure_services() {
    // parity: rpc:structure.JettisonStructureFuel, rpc:structureAssetSafety.MoveCorpAssetsToSafety, rpc:structureAssetSafety.MoveEjectTimeGM, rpc:structureCargoDelivery.DropOffItems, rpc:structureCompressionMgr.CompressItemInStructure
    // parity: rpc:structureCompressionMgr.DecompressGasInStructure, rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency, rpc:structureControl.BoardStructure, rpc:structureControl.EjectFromStructure, rpc:structureControl.GetStructurePilot
    // parity: rpc:structureControl.ReleaseControl, rpc:structureControl.TakeControl, rpc:structureDeliveries.Deliver, rpc:structureDeployment.Anchor, rpc:structureDeployment.CancelUnanchor
    // parity: rpc:structureDeployment.RenameStructure, rpc:structureDeployment.Unanchor, rpc:structureDirectory.GetJumpBridgesWithMyAccess, rpc:structureDirectory.GetNearbyJumpBridges, rpc:structureDirectory.GetStructureDescription
    // parity: rpc:structureDirectory.SetStructureDescription, rpc:structureDocking.Dock, rpc:structureDocking.Undock, rpc:structureGuests.GetGuests, rpc:structureHangarViewMgr.GetMyHangarViewState
    // parity: rpc:structureJumpBridgeMgr.CmdJumpThroughStructureStargate, rpc:structureJumpBridgeMgr.GetJbStructureDestination, rpc:structureJumpBridgeMgr.GetLinkedStructure, rpc:structureVulnerability.UpdateReinforceTimesFromClient
    eve_testkit::attest(&["rpc:structure.JettisonStructureFuel", "rpc:structureAssetSafety.MoveCorpAssetsToSafety", "rpc:structureAssetSafety.MoveEjectTimeGM", "rpc:structureCargoDelivery.DropOffItems", "rpc:structureCompressionMgr.CompressItemInStructure", "rpc:structureCompressionMgr.DecompressGasInStructure", "rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency", "rpc:structureControl.BoardStructure", "rpc:structureControl.EjectFromStructure", "rpc:structureControl.GetStructurePilot", "rpc:structureControl.ReleaseControl", "rpc:structureControl.TakeControl", "rpc:structureDeliveries.Deliver", "rpc:structureDeployment.Anchor", "rpc:structureDeployment.CancelUnanchor", "rpc:structureDeployment.RenameStructure", "rpc:structureDeployment.Unanchor", "rpc:structureDirectory.GetJumpBridgesWithMyAccess", "rpc:structureDirectory.GetNearbyJumpBridges", "rpc:structureDirectory.GetStructureDescription", "rpc:structureDirectory.SetStructureDescription", "rpc:structureDocking.Dock", "rpc:structureDocking.Undock", "rpc:structureGuests.GetGuests", "rpc:structureHangarViewMgr.GetMyHangarViewState", "rpc:structureJumpBridgeMgr.CmdJumpThroughStructureStargate", "rpc:structureJumpBridgeMgr.GetJbStructureDestination", "rpc:structureJumpBridgeMgr.GetLinkedStructure", "rpc:structureVulnerability.UpdateReinforceTimesFromClient"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // structure.JettisonStructureFuel
    let res = rpc(&ctx, &mut sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(10_000_001), PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "structure.JettisonStructureFuel");
    // structureAssetSafety.MoveCorpAssetsToSafety
    let res = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureAssetSafety.MoveCorpAssetsToSafety");
    // structureAssetSafety.MoveEjectTimeGM
    let res = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "structureAssetSafety.MoveEjectTimeGM");
    // structureCargoDelivery.DropOffItems
    let res = rpc(&ctx, &mut sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(10_000_001), PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("MustBeInSpace"), "structureCargoDelivery.DropOffItems");
    // structureCompressionMgr.CompressItemInStructure
    let res = rpc(&ctx, &mut sess, "structureCompressionMgr", "CompressItemInStructure", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("MustBeInStructure"), "structureCompressionMgr.CompressItemInStructure");
    // structureCompressionMgr.DecompressGasInStructure
    let res = rpc(&ctx, &mut sess, "structureCompressionMgr", "DecompressGasInStructure", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("MustBeInStructure"), "structureCompressionMgr.DecompressGasInStructure");
    // structureCompressionMgr.GetMyGasDecompressionEfficiency
    let res = rpc(&ctx, &mut sess, "structureCompressionMgr", "GetMyGasDecompressionEfficiency", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Real(0.8)), "structureCompressionMgr.GetMyGasDecompressionEfficiency");
    // structureControl.BoardStructure
    let res = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureControl.BoardStructure");
    // structureControl.EjectFromStructure
    let res = rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("NotControllingStructure"), "structureControl.EjectFromStructure");
    // structureControl.GetStructurePilot
    let res = rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "structureControl.GetStructurePilot");
    // structureControl.ReleaseControl
    let res = rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("NotControllingStructure"), "structureControl.ReleaseControl");
    // structureControl.TakeControl
    let res = rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureControl.TakeControl");
    // structureDeliveries.Deliver
    let res = rpc(&ctx, &mut sess, "structureDeliveries", "Deliver", &[PyRep::Integer(10_000_001), PyRep::List(Vec::new())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("MissingItemId"), "structureDeliveries.Deliver");
    // structureDeployment.Anchor
    let res = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(35832), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("ShipDocked"), "structureDeployment.Anchor");
    // structureDeployment.CancelUnanchor
    let res = rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureDeployment.CancelUnanchor");
    // structureDeployment.RenameStructure
    let res = rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(10_000_001), PyRep::String("Citadel Alpha".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureDeployment.RenameStructure");
    // structureDeployment.Unanchor
    let res = rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureDeployment.Unanchor");
    // structureDirectory.GetJumpBridgesWithMyAccess
    let res = rpc(&ctx, &mut sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "structureDirectory.GetJumpBridgesWithMyAccess");
    // structureDirectory.GetNearbyJumpBridges
    let res = rpc(&ctx, &mut sess, "structureDirectory", "GetNearbyJumpBridges", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "structureDirectory.GetNearbyJumpBridges");
    // structureDirectory.GetStructureDescription
    let res = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureDirectory.GetStructureDescription");
    // structureDirectory.SetStructureDescription
    let res = rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(10_000_001), PyRep::String("Citadel Description".into())]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureDirectory.SetStructureDescription");
    // structureDirectory.clear_memoized
    let res = rpc(&ctx, &mut sess, "structureDirectory", "clear_memoized", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "structureDirectory.clear_memoized");
    // structureDocking.Dock
    let res = rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("AlreadyDocked"), "structureDocking.Dock");
    // structureDocking.Undock
    let res = rpc(&ctx, &mut sess, "structureDocking", "Undock", &[]).await;
    assert_eq!(res.is_ok(), true, "structureDocking.Undock");
    // structureGuests.GetGuests
    let res = rpc(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "structureGuests.GetGuests");
    // structureHangarViewMgr.GetMyHangarViewState
    let res = rpc(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("NotDockedInStructure"), "structureHangarViewMgr.GetMyHangarViewState");
    // structureJumpBridgeMgr.CmdJumpThroughStructureStargate
    let res = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "CmdJumpThroughStructureStargate", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("StructureNotFound"), "structureJumpBridgeMgr.CmdJumpThroughStructureStargate");
    // structureJumpBridgeMgr.GetJbStructureDestination
    let res = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("NoLinkedStructure"), "structureJumpBridgeMgr.GetJbStructureDestination");
    // structureJumpBridgeMgr.GetLinkedStructure
    let res = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(10_000_001)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("NoLinkedStructure"), "structureJumpBridgeMgr.GetLinkedStructure");
    // structureVulnerability.UpdateReinforceTimesFromClient
    let res = rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(18), PyRep::Integer(2)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("InvalidReinforceSchedule"), "structureVulnerability.UpdateReinforceTimesFromClient");
}

#[tokio::test]
async fn test_batch_b2_standings_and_corp_station() {
    // parity: rpc:corpStationMgr.DoStandingCheckForStationService, rpc:corpStationMgr.MoveCorpHQHere, rpc:killRightMgr.ActivateKillRight, rpc:killRightMgr.BuyKillRight, rpc:peaceTreatyMgr.GetPeaceTreatiesForSession
    // parity: rpc:standingMgr.GetCharStandings, rpc:standingMgr.GetCorpStandings, rpc:standingMgr.GetNPCNPCStandings, rpc:standingMgr.GetStandingCompositions, rpc:standingMgr.GetStandingTransactions
    eve_testkit::attest(&["rpc:corpStationMgr.DoStandingCheckForStationService", "rpc:corpStationMgr.MoveCorpHQHere", "rpc:killRightMgr.ActivateKillRight", "rpc:killRightMgr.BuyKillRight", "rpc:peaceTreatyMgr.GetPeaceTreatiesForSession", "rpc:standingMgr.GetCharStandings", "rpc:standingMgr.GetCorpStandings", "rpc:standingMgr.GetNPCNPCStandings", "rpc:standingMgr.GetStandingCompositions", "rpc:standingMgr.GetStandingTransactions"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // corpStationMgr.DoStandingCheckForStationService
    let res = rpc(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(true)), "corpStationMgr.DoStandingCheckForStationService");
    // corpStationMgr.MoveCorpHQHere
    let res = rpc(&ctx, &mut sess, "corpStationMgr", "MoveCorpHQHere", &[]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("CorporationNotFound"), "corpStationMgr.MoveCorpHQHere");
    // corpStationMgr.SetSessionCheck
    let res = rpc(&ctx, &mut sess, "corpStationMgr", "SetSessionCheck", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "corpStationMgr.SetSessionCheck");
    // corpStationMgr.Unbind
    let res = rpc(&ctx, &mut sess, "corpStationMgr", "Unbind", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::None), "corpStationMgr.Unbind");
    // killRightMgr.ActivateKillRight
    let res = rpc(&ctx, &mut sess, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(P2)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("KillRightNotFound"), "killRightMgr.ActivateKillRight");
    // killRightMgr.BuyKillRight
    let res = rpc(&ctx, &mut sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(P2)]).await;
    assert_eq!(res.as_ref().map_err(|e| e.code.as_str()), Err("KillRightNotFound"), "killRightMgr.BuyKillRight");
    // peaceTreatyMgr.GetPeaceTreatiesForSession
    let res = rpc(&ctx, &mut sess, "peaceTreatyMgr", "GetPeaceTreatiesForSession", &[]).await;
    assert_eq!(res.is_ok(), true, "peaceTreatyMgr.GetPeaceTreatiesForSession");
    // standingMgr.GetCharStandings
    let res = rpc(&ctx, &mut sess, "standingMgr", "GetCharStandings", &[]).await;
    assert_eq!(res.is_ok(), true, "standingMgr.GetCharStandings");
    // standingMgr.GetCorpStandings
    let res = rpc(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[]).await;
    assert_eq!(res.is_ok(), true, "standingMgr.GetCorpStandings");
    // standingMgr.GetNPCNPCStandings
    let res = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[]).await;
    assert_eq!(res.is_ok(), true, "standingMgr.GetNPCNPCStandings");
    // standingMgr.GetStandingCompositions
    let res = rpc(&ctx, &mut sess, "standingMgr", "GetStandingCompositions", &[]).await;
    assert_eq!(matches!(res, Ok(PyRep::List(_))), true, "standingMgr.GetStandingCompositions");
    // standingMgr.GetStandingTransactions
    let res = rpc(&ctx, &mut sess, "standingMgr", "GetStandingTransactions", &[]).await;
    assert_eq!(res.is_ok(), true, "standingMgr.GetStandingTransactions");
}

#[tokio::test]
async fn test_batch_b2_dungeon_and_missions() {
    // parity: rpc:dungeon.DEGetArchetypes, rpc:dungeon.DEGetDungeons, rpc:dungeon.DEGetFactions, rpc:dungeon.DEGetRoomObjectPaletteData, rpc:dungeon.DEGetRooms
    // parity: rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails, rpc:epicArcStatus.AgentHasEpicMissionsForCharacter, rpc:missionTrackerMgr.GetAllMissionObjectives, rpc:missionTrackerMgr.GetMissionInfoItems, rpc:missionTrackerMgr.IsInActiveDungeonID
    // parity: rpc:missionTrackerMgr.UpdateAllMissions
    eve_testkit::attest(&["rpc:dungeon.DEGetArchetypes", "rpc:dungeon.DEGetDungeons", "rpc:dungeon.DEGetFactions", "rpc:dungeon.DEGetRoomObjectPaletteData", "rpc:dungeon.DEGetRooms", "rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails", "rpc:epicArcStatus.AgentHasEpicMissionsForCharacter", "rpc:missionTrackerMgr.GetAllMissionObjectives", "rpc:missionTrackerMgr.GetMissionInfoItems", "rpc:missionTrackerMgr.IsInActiveDungeonID", "rpc:missionTrackerMgr.UpdateAllMissions"]);

    let ctx = ServerContext::new();
    let mut sess = setup_pilot(&ctx, 100_001, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_ADMIN | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_GMH;

    // dungeon.DEGetArchetypes
    let res = rpc(&ctx, &mut sess, "dungeon", "DEGetArchetypes", &[]).await;
    assert_eq!(res.is_ok(), true, "dungeon.DEGetArchetypes");
    // dungeon.DEGetDungeons
    let res = rpc(&ctx, &mut sess, "dungeon", "DEGetDungeons", &[]).await;
    assert_eq!(res.is_ok(), true, "dungeon.DEGetDungeons");
    // dungeon.DEGetFactions
    let res = rpc(&ctx, &mut sess, "dungeon", "DEGetFactions", &[]).await;
    assert_eq!(res.is_ok(), true, "dungeon.DEGetFactions");
    // dungeon.DEGetRoomObjectPaletteData
    let res = rpc(&ctx, &mut sess, "dungeon", "DEGetRoomObjectPaletteData", &[]).await;
    assert_eq!(res.is_ok(), true, "dungeon.DEGetRoomObjectPaletteData");
    // dungeon.DEGetRooms
    let res = rpc(&ctx, &mut sess, "dungeon", "DEGetRooms", &[]).await;
    assert_eq!(res.is_ok(), true, "dungeon.DEGetRooms");
    // dungeonExplorationMgr.GetMyEscalatingPathDetails
    let res = rpc(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "dungeonExplorationMgr.GetMyEscalatingPathDetails");
    // epicArcStatus.AgentHasEpicMissionsForCharacter
    let res = rpc(&ctx, &mut sess, "epicArcStatus", "AgentHasEpicMissionsForCharacter", &[PyRep::Integer(3011867)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "epicArcStatus.AgentHasEpicMissionsForCharacter");
    // missionTrackerMgr.GetAllMissionObjectives
    let res = rpc(&ctx, &mut sess, "missionTrackerMgr", "GetAllMissionObjectives", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "missionTrackerMgr.GetAllMissionObjectives");
    // missionTrackerMgr.GetMissionInfoItems
    let res = rpc(&ctx, &mut sess, "missionTrackerMgr", "GetMissionInfoItems", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "missionTrackerMgr.GetMissionInfoItems");
    // missionTrackerMgr.IsInActiveDungeonID
    let res = rpc(&ctx, &mut sess, "missionTrackerMgr", "IsInActiveDungeonID", &[PyRep::Integer(1)]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::Bool(false)), "missionTrackerMgr.IsInActiveDungeonID");
    // missionTrackerMgr.UpdateAllMissions
    let res = rpc(&ctx, &mut sess, "missionTrackerMgr", "UpdateAllMissions", &[]).await;
    assert_eq!(res.as_ref().ok(), Some(&PyRep::List(vec![])), "missionTrackerMgr.UpdateAllMissions");
}

