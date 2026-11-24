//! Central sorted declaration of read-only `(service, method)` RPC pairs (`#1194` Track 3B-4).
//!
//! Entries listed here mark handler-audited read-only queries (`RpcAccess::Read`) whose
//! handlers take no DB write path (`with_mut_snapshot`, `try_with_mut_snapshot`, `db.tx`,
//! `with_store`, lazy DB sync/purge helpers, or runtime/cache/notification mutations). Unlisted
//! entries default to `RpcAccess::Write`. A module being edited anyway may alternatively mark
//! its entry with `.read_only()` in place, but declaring the same pair in both places is rejected
//! when [`crate::router::rpc_registry::RpcRegistry`] is built.

/// Sorted list of handler-audited read-only `(service, method)` RPC query pairs (`333` entries).
pub const READ_ONLY_RPCS: &[(&str, &str)] = &[
    ("FastCheckoutService", "GetTestingConfiguration"),
    ("FastCheckoutService", "IsFastCheckoutEnabledForUser"),
    ("FastCheckoutService", "get_tutorial_completion_status"),
    ("FastCheckoutService", "is_enabled"),
    ("LPStoreMgr", "GetAvailableOffersFromCorp"),
    ("LPSvc", "GetAllMyCharacterWalletLPBalances"),
    ("LPSvc", "GetAllMyCorporationWalletLPBalances"),
    ("RWManager", "get_closest_rw_stations"),
    ("RWManager", "solarsystem_contains_rw_instances"),
    ("accessGroupBookmarkMgr", "GetFolderInfo"),
    ("accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess"),
    ("account", "GetEntryTypes"),
    ("account", "GetTransactions"),
    ("activityMgr", "GetActivitiesPopState"),
    ("activityMgr", "GetActivityBadgeStatuses"),
    ("agentMgr", "GetCompletedCareerAgentIDs"),
    ("agentMgr", "GetDisabledMissions"),
    ("agentMgr", "GetMessagesForEpicArcMissions"),
    ("agentMgr", "GetMyEpicArcStatus"),
    ("agentMgr", "GetPlausibleMissionIDs"),
    ("agentMgr", "GetReplayTimestamp"),
    ("agentMgr", "agentID"),
    ("agentMgr", "corporationID"),
    ("agentMgr", "factionID"),
    ("agentMgr", "solarsystemID"),
    ("agentMgr", "stationID"),
    ("air_npe", "get_air_npe_state"),
    ("air_npe", "is_air_npe_enabled"),
    ("alert", "BeanCount"),
    ("alert", "GetLogModeForError"),
    ("allianceRegistry", "GetAllianceBulletins"),
    ("allianceRegistry", "GetAllianceContacts"),
    ("allianceRegistry", "GetAllianceSovereigntyStructuresInfo"),
    ("allianceRegistry", "GetBillBalance"),
    ("allianceRegistry", "GetBills"),
    ("allianceRegistry", "GetBillsReceivable"),
    ("allianceRegistry", "GetBulletins"),
    ("allianceRegistry", "GetCapitalSystemInfo"),
    ("allianceRegistry", "GetLabels"),
    ("allianceRegistry", "GetPrimeTimeInfo"),
    ("allianceRegistry", "GetRankedAlliances"),
    ("alphaInjectorMgr", "GetNextAvailableInjection"),
    ("authentication", "Ping"),
    ("bannedwords", "check_chat_words_allowed"),
    ("bannedwords", "check_search_words_allowed"),
    ("bannedwords", "check_words_allowed"),
    ("bannedwords", "fetch_regexes"),
    ("beyonce", "GetCynoJammerState"),
    ("beyonce", "GetFleetState"),
    ("beyonce", "GetRelativity"),
    ("blueprintManager", "GetBlueprintData"),
    ("blueprintManager", "GetBlueprintDataByOwner"),
    ("blueprintManager", "GetLimits"),
    ("bookmark", "GetFolders"),
    ("bountyProxy", "GetBounties"),
    ("bountyProxy", "GetBountiesAndKillRights"),
    ("bountyProxy", "GetBounty"),
    ("bountyProxy", "GetKillRightsOnCharacters"),
    ("bountyProxy", "GetMyBounties"),
    ("bountyProxy", "GetMyKillRights"),
    ("bountyProxy", "GetTopAllianceBounties"),
    ("bountyProxy", "GetTopAllianceBountyHunters"),
    ("bountyProxy", "GetTopBountyHunters"),
    ("bountyProxy", "GetTopCorpBounties"),
    ("bountyProxy", "GetTopCorporationBountyHunters"),
    ("bountyProxy", "GetTopPilotBounties"),
    ("bountyProxy", "GetTopPilotBountyHunters"),
    ("bountyProxy", "SearchAllianceBounties"),
    ("bountyProxy", "SearchBounties"),
    ("bountyProxy", "SearchCharBounties"),
    ("bountyProxy", "SearchCorpBounties"),
    ("browserLockdownSvc", "GetDefaultHomePage"),
    ("browserLockdownSvc", "GetFlaggedSitesHash"),
    ("browserLockdownSvc", "GetFlaggedSitesList"),
    ("browserLockdownSvc", "IsBrowserInLockdown"),
    ("certificateMgr", "GetCertificateCategories"), // rpc-lint: allow crucible legacy service certificateMgr in rpc_service_catalog.json
    ("certificateMgr", "GetCertificateClasses"), // rpc-lint: allow crucible legacy service certificateMgr in rpc_service_catalog.json
    ("certificateMgr", "GetCertificatesByCharacter"), // rpc-lint: allow crucible legacy service certificateMgr in rpc_service_catalog.json
    ("certificateMgr", "GetMyCertificates"), // rpc-lint: allow crucible legacy service certificateMgr in rpc_service_catalog.json
    ("charFittingMgr", "GetFittings"),
    ("charMgr", "GetAssetWorth"),
    ("charMgr", "GetCharacterDescription"),
    ("charMgr", "GetContactList"),
    ("charMgr", "GetContacts"),
    ("charMgr", "GetEmployementRecordAndCharacterTransfers"),
    ("charMgr", "GetLabels"),
    ("charMgr", "GetOwnerNote"),
    ("charMgr", "GetOwnerNoteLabels"),
    ("charMgr", "GetPaperdollState"),
    ("charMgr", "GetPrivateInfo"),
    ("charMgr", "GetPublicInfo3"),
    ("charMgr", "GetSettingsInfo"),
    ("charMgr", "ListStations"),
    ("charUnboundMgr", "GetCharOmegaDowngradeStatus"),
    ("charUnboundMgr", "GetCohortsForUser"),
    ("charUnboundMgr", "GetValidRandomName"),
    ("charUnboundMgr", "ListStations"),
    ("charUnboundMgr", "ValidateNameEx"),
    ("colonyResourcesMgr", "GetMyCorporationSkyhooks"),
    ("config", "GetAverageMarketPrices"),
    ("config", "GetBlackListedPlanets"),
    ("config", "GetLocationsEx"),
    ("config", "GetMultiAllianceShortNamesEx"),
    ("config", "GetMultiCorpTickerNamesEx"),
    ("config", "GetMultiLocationsEx"),
    ("config", "GetMultiOwnersEx"),
    ("config", "GetOldStationData"),
    ("config", "GetStationSolarSystemsByOwner"),
    ("contractProxy", "GetLoginInfo"),
    ("corpFittingMgr", "GetCommunityFittings"),
    ("corpFittingMgr", "GetFittings"),
    ("corpMiningLedger", "GetObserverLedger"),
    ("corpMiningLedger", "GetObserversWithMiningEvents"),
    ("corpRegistry", "GetEveOwners"),
    ("corpRegistry", "GetInfoWindowDataForChar"),
    ("corpRegistry", "GetMyApplications"),
    ("corpRegistry", "GetMyOldApplications"),
    ("corpRegistry", "GetRecentKills"),
    ("corpRegistry", "GetRecentLosses"),
    ("corpRegistry", "GetSuggestedAllianceShortNames"),
    ("corpRegistry", "GetSuggestedTickerNames"),
    ("corpmgr", "GetCorporationIDForCharacter"),
    ("corporationSvc", "GetEmployementRecordAndCharacterTransfers"),
    ("crimewatch", "GetCharacterSecurityStatus"),
    ("crimewatch", "GetMySecurityStatus"),
    ("devIndexManager", "GetAllDevelopmentIndices"),
    ("devIndexManager", "GetDevelopmentIndicesForSystem"),
    ("dogmaIM", "GetMyCertificates"),
    ("dogmaIM", "GetRequiredSkillLevels"),
    ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances"),
    ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances"),
    ("dynamicItemService", "GetDynamicItemInfo"),
    ("dynamicResourceCacheMgr", "GetDBSMapData"),
    ("dynamicResourceCacheMgr", "GetDynamicResourceSettings"),
    ("dynamicResourceCacheMgr", "GetESSAgencyData"),
    ("dynamicResourceCacheMgr", "GetESSReserveBankKeys"),
    ("dynamicResourceCacheMgr", "GetESSSystemDetails"),
    ("dynamicResourceCacheMgr", "get"),
    ("dynamicResourceCacheMgr", "iteritems"),
    ("encodedItems", "DecodeItem"),
    ("epicArcStatus", "AgentHasEpicMissionsForCharacter"),
    ("expertSystemMgr", "GetMyExpertSystems"),
    ("facWarMgr", "CheckForRankChange"),
    ("facWarMgr", "GetAllianceWarFactionID"),
    ("facWarMgr", "GetCharacterRankInfo"),
    ("facWarMgr", "GetControlBunkerIDInSystem"),
    ("facWarMgr", "GetCorpFactionalWarStatus"),
    ("facWarMgr", "GetCorporationWarFactionID"),
    ("facWarMgr", "GetFactionCorporations"),
    ("facWarMgr", "GetFactionMilitiaCorporation"),
    ("facWarMgr", "GetMyCharacterRankInfo"),
    ("facWarMgr", "GetMyCharacterRankOverview"),
    ("facWarMgr", "GetSolarSystemLPs"),
    ("facWarMgr", "GetStats_Alliance"),
    ("facWarMgr", "GetStats_Character"),
    ("facWarMgr", "GetStats_Corp"),
    ("facWarMgr", "GetStats_CorpPilots"),
    ("facWarMgr", "GetStats_FactionInfo"),
    ("facWarMgr", "GetStats_Militia"),
    ("facWarMgr", "GetStats_TopAndAllKillsAndVPs"),
    ("facilityManager", "GetFacilities"),
    ("facilityManager", "GetFacilitiesByID"),
    ("facilityManager", "GetFacility"),
    ("facilityManager", "GetFacilityLocations"),
    ("facilityManager", "GetMaxActivityModifiers"),
    ("fleetMgr", "GetBroadcastFilter"), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    ("fleetObjectHandler", "GetBroadcastFilter"), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    ("fleetProxy", "GetAvailableFleetAds"),
    ("fleetProxy", "GetFleetAdsForChar"),
    ("fleetProxy", "GetMyFleetFinderAdvert"),
    ("fwCharacterEnlistmentMgr", "GetCorpAllowedEnlistmentFactions"),
    ("fwCharacterEnlistmentMgr", "GetMyEnlistment"),
    ("fwVictoryPointMgr", "GetAllVictoryPointStates"),
    ("fwVictoryPointMgr", "GetLocalVictoryPointState"),
    ("fwWarzoneSolarsystem", "GetAllWarzones"),
    ("fwWarzoneSolarsystem", "GetHQSystemIDs"),
    ("fwWarzoneSolarsystem", "GetLocalOccupationState"),
    ("fwWarzoneSolarsystem", "iteritems"),
    ("fwWarzoneSolarsystem", "itervalues"),
    ("fwWarzoneSolarsystem", "keys"),
    ("home_station", "get_next_remote_change_time"),
    ("incursion", "GetIncursionLPLog"), // rpc-lint: allow incursion LP log query adjacent to home_station in READ_ONLY_RPCS
    ("industryManager", "GetJob"),
    ("industryManager", "GetJobCounts"),
    ("industryManager", "GetJobsByOwner"),
    ("infoGatheringMgr", "GetStateAndConfig"),
    ("insuranceSvc", "GetInsurancePrice"),
    ("insuranceSvc", "GetInsurancePrices"),
    ("insurgencySolarsystem", "GetAllVisibleCampaigns"),
    ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot"),
    ("invbroker", "GetAssetWorth"),
    ("invbroker", "GetItemDescriptor"),
    ("invbroker", "GetPriceForItems"),
    ("invbroker", "HasExistingPasswordSet"),
    ("invbroker", "ListStations"),
    ("itemLocking", "GetItemsByLocation"),
    ("itemLocking", "GetLockedItemLocations"),
    ("jumpCloneSvc", "GetCloneState"),
    ("jumpCloneSvc", "GetShipCloneState"),
    ("jumpCloneSvc", "GetStationCloneState"),
    ("jumpCloneSvc", "ValidateInstallJumpClone"),
    ("loginCampaignManager", "get_client_campaign_state"),
    ("lookupSvc", "LookupCharacters"),
    ("lookupSvc", "LookupCorporations"),
    ("lookupSvc", "LookupEvePlayerCharacters"),
    ("lookupSvc", "LookupFactions"),
    ("lookupSvc", "LookupKnownLocationsByGroup"),
    ("lookupSvc", "LookupLocationsByGroup"),
    ("lookupSvc", "LookupNoneNPCAccountOwners"),
    ("lookupSvc", "LookupOwners"),
    ("lookupSvc", "LookupPCOwners"),
    ("lookupSvc", "LookupWarableCorporationsOrAlliances"),
    ("map", "GetConstellationLPData"),
    ("map", "GetCurrentSovData"),
    ("map", "GetDeadspaceAgentsMap"),
    ("map", "GetDeadspaceComplexMap"),
    ("map", "GetEdencomFortressSystems"),
    ("map", "GetEdencomMinorVictorySystems"),
    ("map", "GetFacWarZoneInfo"),
    ("map", "GetHistory"),
    ("map", "GetIncursionGlobalReport"),
    ("map", "GetIndustryCostModifier"),
    ("map", "GetIndustryJobsOverLast24Hours"),
    ("map", "GetMyExtraMapInfo"),
    ("map", "GetMyExtraMapInfoAgents"),
    ("map", "GetRecentSovActivity"),
    ("map", "GetSecurityModifiedSystems"),
    ("map", "GetSolarSystemVisits"),
    ("map", "GetStationCount"),
    ("map", "GetStationInfo"),
    ("map", "GetSystemsInIncursions"),
    ("map", "GetTriglavianMinorVictorySystems"),
    ("marketProxy", "CharGetTransactions"),
    ("marketProxy", "CorpGetTransactions"),
    ("marketProxy", "GetCharEscrow"),
    ("marketProxy", "GetCharOrders"),
    ("marketProxy", "GetMarketGroupTypes"), // rpc-lint: allow legacy marketProxy method migrated 1:1 from market_trading::handle
    ("marketProxy", "GetMarketGroups"),
    ("marketProxy", "GetNewPriceHistory"),
    ("marketProxy", "GetOldPriceHistory"),
    ("marketProxy", "GetTypesInMarketGroup"), // rpc-lint: allow legacy marketProxy method migrated 1:1 from market_trading::handle
    ("missionTrackerMgr", "IsInActiveDungeonID"),
    ("moonExtractions", "GetExtractionAndEventIDFromStructureID"),
    ("moonExtractions", "GetExtractionsForCorp"),
    ("moonExtractions", "GetMoonResourcesForStructure"),
    ("nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections"),
    ("notificationMgr", "GetAllNotifications"),
    ("notificationMgr", "GetByGroupID"),
    ("notificationMgr", "GetUnprocessed"),
    ("onlineStatus", "GetInitialState"),
    ("onlineStatus", "GetOnlineStatus"),
    ("operationsManager", "can_character_play_the_tutorial"),
    ("operationsManager", "get_active_category_id"),
    ("operationsManager", "get_character_progress"),
    ("operationsManager", "get_mission_avoidance_systems"),
    ("operationsManager", "get_operations_completed_at_least_once"),
    ("operationsManager", "get_tutorial_state"),
    ("operationsManager", "has_skipped_tutorial"),
    ("operationsManager", "is_category_complete"),
    ("operationsManager", "is_main_tutorial_finished"),
    ("ownerGroupManager", "GetGroupsMany"),
    ("ownerGroupManager", "GetMyGroups"),
    ("paperDollServer", "GetMyPaperDollData"),
    ("paperDollServer", "GetPaperDollData"),
    ("paperDollServer", "GetPaperDollDataBatch"),
    ("paperDollServer", "GetPaperDollPortraitDataFor"),
    ("petitioner", "GetCategories"),
    ("petitioner", "GetCategoryHierarchicalInfo"),
    ("petitioner", "GetCategoryProperties"),
    ("petitioner", "GetClaimedPetitions"),
    ("petitioner", "GetClientPickerInfo"),
    ("petitioner", "GetLog"),
    ("petitioner", "GetMyPetitionsEx"),
    ("petitioner", "GetPetitionMessages"),
    ("petitioner", "GetPetitionQueue"),
    ("petitioner", "GetQueues"),
    ("petitioner", "GetUnreadMessages"),
    ("petitioner", "GetUserCatalogCountry"),
    ("petitioner", "HasOpenTickets"),
    ("planetMgr", "GMGetCompleteResource"),
    ("planetMgr", "GMGetLocalDistributionReport"),
    ("planetMgr", "GMGetSynchedServerState"),
    ("planetMgr", "GetCommandCenterLevel"),
    ("planetMgr", "GetCommandPinsForPlanet"),
    ("planetMgr", "GetExtractorsForPlanet"),
    ("planetMgr", "GetFullNetworkForOwner"),
    ("planetOrbitalRegistryBroker", "GetSettingsInfo"),
    ("planetOrbitalRegistryBroker", "GetTaxRate"),
    ("publicQaToolsServer", "CanGiveItemForMultifit"),
    ("pvpFilamentMgr", "GetActiveEvents"),
    ("pvpFilamentMgr", "GetAllEvents"),
    ("pvpFilamentMgr", "GetCharacterStatistics"),
    ("pvpFilamentMgr", "GetLeaderboard"),
    ("pvpFilamentMgr", "GetMostRecentEvent"),
    ("pvpFilamentMgr", "GetNextEventDate"),
    ("raffleProxy", "AmIBanned"),
    ("rewardMgr", "GetRewardLPLogs"),
    ("search", "Query"),
    ("search", "QuickQuery"),
    ("seasonManager", "challenge_is_dormant"),
    ("seasonManager", "get_challenge"),
    ("seasonManager", "get_challenge_expiration_date"),
    ("seasonManager", "get_max_active_challenges"),
    ("seasonManager", "get_season_end_time"),
    ("seasonalLoginCampaignManager", "get_active_campaign"),
    ("seasonalLoginCampaignManager", "get_claim_history"),
    ("securityMgr", "get_modified_systems"),
    ("ship", "GetDirtTimestamp"),
    ("ship", "GetShipFittingInfo"),
    ("skillMgr2", "GetAttributes"),
    ("skillMgr2", "GetImplants"),
    ("skillMgr2", "GetRespecInfo"),
    ("skillMgr2", "GetSkillHistory"),
    ("sovMgr", "IsOnLocalSovHubFuelAccessGroup"),
    ("standingMgr", "GetCharStandings"),
    ("standingMgr", "GetCorpStandings"),
    ("standingMgr", "GetNPCNPCStandings"),
    ("standingMgr", "GetStandingCompositions"),
    ("standingMgr", "GetStandingTransactions"),
    ("storeManager", "GetCategories"),
    ("storeManager", "GetProducts"),
    ("storeManager", "get_categories"),
    ("storeManager", "get_offers"),
    ("storeManager", "get_products"),
    ("structureCynoBeaconMgr", "GetMyAccessibleOnlineCynoBeaconStructures"),
    ("survey", "GetActiveSurveyID"),
    ("taleMgr", "GetGlobalWorldEventTales"),
    ("taleMgr", "get_active_tales_by_template"),
    ("tourneyMgr", "GetMatchSystem"),
    ("tourneyMgr", "GetPotentialMatches"),
    ("tourneyMgr", "GetTeamMembers"),
    ("tourneyMgr", "GetTourneySetup"),
    ("tourneyMgr", "QueryShipList"),
];
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};

    use super::*;
    use crate::router::context::ServerContext;
    use crate::router::rpc_registry::{
        rpc_entry, rpc_entry_with_caller, RpcAccess, RpcCallerClass, RpcDispatchHandler, RpcEntry,
        RpcRegistry,
    };

    fn noop_handler(
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
    fn read_only_rpcs_list_is_sorted_and_unique_and_builds_default_registry() {
        for pair in READ_ONLY_RPCS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "READ_ONLY_RPCS must be strictly sorted and unique; found {:?} before {:?}",
                pair[0],
                pair[1]
            );
        }

        let reg = RpcRegistry::build();
        let read_set: std::collections::HashSet<(&str, &str)> =
            READ_ONLY_RPCS.iter().copied().collect();
        for entry in reg.all_entries() {
            let expected = if read_set.contains(&(entry.service, entry.method)) {
                RpcAccess::Read
            } else {
                RpcAccess::Write
            };
            assert_eq!(
                entry.access, expected,
                "unexpected access for {}.{}",
                entry.service, entry.method
            );
        }
        for &(svc, meth) in READ_ONLY_RPCS {
            let entry = reg
                .lookup(svc, meth)
                .unwrap_or_else(|| panic!("missing compiled entry for {svc}.{meth}"));
            assert_eq!(entry.access, RpcAccess::Read);
        }
    }

    #[test]
    fn read_set_validation_rejects_missing_duplicate_and_double_declared_entries() {
        static TEST_ENTRIES: &[RpcEntry] = &[
            rpc_entry(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadViaList",
                RpcDispatchHandler::Sync(noop_handler),
            ),
            rpc_entry_with_caller(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadInPlace",
                RpcCallerClass::Account,
                RpcDispatchHandler::Sync(noop_handler),
            )
            .read_only(),
            rpc_entry(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "DefaultWrite",
                RpcDispatchHandler::Sync(noop_handler),
            ),
        ];

        // Listed pair comes back Read; in-place .read_only() comes back Read; unlisted is Write.
        let reg = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadViaList",
            )],
        )
        .expect("valid read set should build");
        assert_eq!(
            reg.lookup("testSvc", "ReadViaList").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Read
        );
        assert_eq!(
            reg.lookup("testSvc", "ReadInPlace").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Read
        );
        assert_eq!(
            reg.lookup("testSvc", "DefaultWrite").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Write
        );

        // Pair with no table entry fails validation.
        let missing_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "NoSuchEntry",
            )],
        )
        .expect_err("missing read-set entry must fail validation");
        assert!(
            missing_err.contains("testSvc.NoSuchEntry"),
            "unexpected error: {missing_err}"
        );

        // Pair listed twice in read_only fails validation.
        let dup_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[
                (
                    "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                    "ReadViaList",
                ),
                (
                    "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                    "ReadViaList",
                ),
            ],
        )
        .expect_err("duplicate read-set entry must fail validation");
        assert!(
            dup_err.contains("testSvc.ReadViaList"),
            "unexpected error: {dup_err}"
        );

        // Pair marked both with .read_only() in place and in read_only fails validation.
        let both_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadInPlace",
            )],
        )
        .expect_err("doubly-declared read-only entry must fail validation");
        assert!(
            both_err.contains("testSvc.ReadInPlace"),
            "unexpected error: {both_err}"
        );
    }
}
