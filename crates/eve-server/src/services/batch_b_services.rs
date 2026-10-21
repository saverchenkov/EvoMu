//! Batch B auxiliary services closure.
//!
//! Covers:
//! - `home_station`: character home station, clone candidates, and remote change cooldowns
//! - `abyssal_content_manager` & `abyssalMgr`: abyssal deadspace generation and activation
//! - `browserLockdownSvc`: in-game browser security and homepage policy
//! - `allianceRegistry`: alliance founding records and binding
//! - `expertSystemMgr`: expert system skill licenses
//! - `hackingMgr`: exploration data/relic minigame utility interactions
//! - `bannedwords`: chat and search word filters
//! - `stationSvc`: station and alliance system queries
//! - `FastCheckoutService`: NES store checkout testing configuration
//! - `RWManager`: Resource Wars instances
//! - `communityTrackMgr`: Community goal attributes
//! - `insuranceSvc`: ship insurance contracts and pricing
//! - `loadService`: node telemetry and configuration
//! - `localizationServer`: dynamic client localization reload
//! - `mutualWarInviteMgr`: mutual war invitation preferences
//! - `populationCap`: system load slot management
//! - `search`: universe search and quick query
//! - `shipCasterLandingPadMgr`, `shipCasterLauncherMgr`, `shipcasterTravelMgr`, `starterShipcasterTravelMgr`: faction shipcaster mechanics
//! - `taleMgr`: dynamic world event tales
//! - `voidSpaceMgr`: void space and gate transitions
//! - Auxiliary services: `agentMgr`, `alert`, `alphaInjectorMgr`, `characterEnergyMgr`, `dogmaIM`,
//!   `dynamicBountyMgr`, `externalQueueMgr`, `keeper`, `monolithTaskTrackerMgr`, `nonDiminishingInjectionMgr`,
//!   `raffleMgr`, `repairSvc`, `reprocessingSvc`, `rewardMgr`, `sessionMgr`, `skillMgr2`,
//!   `structure`, `structureAssetSafety`, `structureSettings`, `tcpRawProxyService`, `warStatisticMgr`,
//!   `warpVectorMgr`, `warsInfoMgr`, `beyonce`, `corpStationMgr`.

#![forbid(unsafe_code)]

use crate::router::context::BoundObjectEntry;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Handles incoming RPC requests across Batch B auxiliary services.
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // --- home_station ---
        ("home_station", "get_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (station_id, solar_sys_id) = ctx
                .db
                .get_character(char_id)
                .ok()
                .map(|ch| (ch.station_id.unwrap_or(60_003_760), ch.solar_system_id))
                .unwrap_or((60_003_760, 30_000_142));
            Some(Ok(PyRep::keyval([
                ("station_id", PyRep::Integer(station_id)),
                ("type_id", PyRep::Integer(1531)),
                ("solar_system_id", PyRep::Integer(solar_sys_id as i64)),
                ("is_fallback", PyRep::Bool(false)),
            ])))
        }

        ("home_station", "get_home_station_candidates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (station_id, solar_sys_id) = ctx
                .db
                .get_character(char_id)
                .ok()
                .map(|ch| (ch.station_id.unwrap_or(60_003_760), ch.solar_system_id))
                .unwrap_or((60_003_760, 30_000_142));
            let candidate = PyRep::keyval([
                ("station_id", PyRep::Integer(station_id)),
                ("type_id", PyRep::Integer(1531)),
                ("solar_system_id", PyRep::Integer(solar_sys_id as i64)),
                ("is_current_station", PyRep::Bool(true)),
                ("is_school_hq", PyRep::Bool(true)),
                ("is_valid", PyRep::Bool(true)),
                (
                    "validation_token",
                    PyRep::String("val_tok_homestation".into()),
                ),
            ]);
            Some(Ok(PyRep::List(vec![candidate])))
        }

        ("home_station", "get_next_remote_change_time") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // 0 indicates the remote home station change cooldown has elapsed
            Some(Ok(PyRep::Integer(0)))
        }

        ("home_station", "reset_remote_change_time") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("home_station", "self_destruct_clone") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        // --- abyssal_content_manager & abyssalMgr ---
        ("abyssal_content_manager", "GenerateAbyssEncounterContent") => {
            let tier = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("tier".into()), PyRep::Integer(tier)),
                (PyRep::String("rooms".into()), PyRep::Integer(3)),
            ])))
        }

        ("abyssal_content_manager", "GenerateContentFromExistingPlan")
        | ("abyssal_content_manager", "GetContentDetailsForShipLocation")
        | ("abyssal_content_manager", "generate_abyss_npc_spawn_report") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(Vec::new())))
        }

        ("abyssalMgr", "AbyssalGateActivation")
        | ("abyssalMgr", "AbyssalEndGateActivation")
        | ("abyssalMgr", "ClientIsReady") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        // --- browserLockdownSvc ---
        ("browserLockdownSvc", "GetDefaultHomePage") => {
            Some(Ok(PyRep::String("https://www.eveonline.com".into())))
        }

        ("browserLockdownSvc", "GetFlaggedSitesHash") => {
            Some(Ok(PyRep::String(
                "d41d8cd98f00b204e9800998ecf8427e".into(),
            )))
        }

        ("browserLockdownSvc", "GetFlaggedSitesList") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("browserLockdownSvc", "IsBrowserInLockdown") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(false)))
        }

        // --- allianceRegistry ---
        ("allianceRegistry", "Bind") | ("allianceRegistry", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("allianceRegistry", "creatorCharID") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Integer(90_000_001)))
        }

        ("allianceRegistry", "creatorCorpID") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Integer(2_000_001)))
        }

        // --- expertSystemMgr ---
        ("expertSystemMgr", "GetMyExpertSystems") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("expertSystemMgr", "ConsumeExpertSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("expertSystemTypeID", PyRep::Integer(57451)),
                ("durationSeconds", PyRep::Integer(604800)),
            ])))
        }

        ("expertSystemMgr", "RemoveMyExpertSystem") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        // --- hackingMgr ---
        ("hackingMgr", "QuitHackingAttempt") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("hackingMgr", "UsedUtilityElementOnVirus") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("success".into()),
                PyRep::Bool(true),
            )])))
        }

        ("hackingMgr", "values") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        // --- bannedwords ---
        ("bannedwords", "check_search_words_allowed")
        | ("bannedwords", "check_words_allowed") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        ("bannedwords", "fetch_regexes") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        // --- stationSvc ---
        ("stationSvc", "GetAllianceSystems") | ("stationSvc", "GetSystemsForAlliance") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("stationSvc", "GetStationsForOwner") => {
            let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut list = Vec::new();
            if owner_id > 0 {
                list.push(PyRep::Integer(60_003_760));
            }
            Some(Ok(PyRep::List(list)))
        }

        // --- FastCheckoutService ---
        ("FastCheckoutService", "ClearFastCheckoutCacheForUser") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("FastCheckoutService", "GetTestingConfiguration") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("enabled".into()),
                PyRep::Bool(true),
            )])))
        }

        // --- RWManager (Resource Wars) ---
        ("RWManager", "get_closest_rw_stations") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("RWManager", "solarsystem_contains_rw_instances") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(false)))
        }

        // --- beyonce ---
        ("beyonce", "Bind") | ("beyonce", "Unbind") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- communityTrackMgr ---
        ("communityTrackMgr", "change_qa_mode") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("communityTrackMgr", "get_instance_attributes_public") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(Vec::new())))
        }

        // --- corpStationMgr ---
        ("corpStationMgr", "SetSessionCheck") | ("corpStationMgr", "Unbind") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- insuranceSvc ---
        ("insuranceSvc", "GetInsurancePrices") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(Vec::new())))
        }

        ("insuranceSvc", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- loadService ---
        ("loadService", "GetTotalStats") => {
            Some(Ok(PyRep::Dict(vec![
                (PyRep::String("cpu".into()), PyRep::Real(0.1)),
                (PyRep::String("memory".into()), PyRep::Real(0.2)),
            ])))
        }

        ("loadService", "SetConfig") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- localizationServer ---
        ("localizationServer", "ReloadFSDPickle")
        | ("localizationServer", "UpdateLocalizationQAWrap") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        // --- mutualWarInviteMgr ---
        ("mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(false)))
        }

        ("mutualWarInviteMgr", "SetInvitesBlockedByPlayer") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- populationCap ---
        ("populationCap", "GetCharacterLoadSlot") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::keyval([("allowed", PyRep::Bool(true))])))
        }

        ("populationCap", "MoveCharacterToNewSystem") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // --- search ---
        ("search", "Query") | ("search", "QuickQuery") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(Vec::new())))
        }

        // --- shipCasterLandingPadMgr & shipCasterLauncherMgr & starterShipcasterTravelMgr & shipcasterTravelMgr ---
        ("shipCasterLandingPadMgr", "GetAllLandingPads")
        | ("shipCasterLandingPadMgr", "GetFactionLandingPads") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("starterShipcasterTravelMgr", "CanCharacterJump") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        ("starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster")
        | ("shipcasterTravelMgr", "CmdJumpThroughShipcaster") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        // --- taleMgr ---
        ("taleMgr", "GetGlobalWorldEventTales")
        | ("taleMgr", "get_active_tales_by_template") => {
            if !session.has_gm_role() {
                return None;
            }
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        // --- voidSpaceMgr ---
        ("voidSpaceMgr", "ClientIsReady")
        | ("voidSpaceMgr", "VoidSpaceEndGateActivation") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        // --- Auxiliary Services Singletons ---
        ("agentMgr", "solarsystemID") => {
            let obj_id = bound_entry.map(|b| b.object_id).unwrap_or(0);
            let sys_id = ctx
                .sde
                .solar_systems
                .keys()
                .next()
                .copied()
                .unwrap_or(30_000_142);
            let final_sys = if obj_id > 0 { obj_id } else { sys_id as i64 };
            Some(Ok(PyRep::Integer(final_sys)))
        }

        ("alert", "GetLogModeForError") => {
            Some(Ok(PyRep::Integer(1)))
        }

        ("alphaInjectorMgr", "GetNextAvailableInjection") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("characterEnergyMgr", "GetMyEnergyState") => {
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("energy".into()),
                PyRep::Real(100.0),
            )])))
        }

        ("dogmaIM", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("dynamicBountyMgr", "GetOutputForClientSolarSystem") => {
            // DBS output multiplier (115% baseline in normal space)
            Some(Ok(PyRep::Real(1.15)))
        }

        ("externalQueueMgr", "PublishProxyEvent") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("keeper", "Bind") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("monolithTaskTrackerMgr", "HasEverWarped") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        ("nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("raffleMgr", "QA_SeedRaffles") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("repairSvc", "SetSessionCheck") | ("reprocessingSvc", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("rewardMgr", "GetDelayedRewardsByGroupIDs") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Dict(Vec::new())))
        }

        ("sessionMgr", "RemoveSessionsFromServer") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("skillMgr2", "GetMySkillHandler") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Integer(char_id)))
        }

        ("structure", "JettisonStructureFuel") => {
            if args.len() < 3 {
                return None;
            }
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("structureAssetSafety", "MoveEjectTimeGM") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("structureSettings", "CharacterHasSetting") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(false)))
        }

        ("tcpRawProxyService", "GetESPTunnelingAddressByNodeID") => {
            Some(Ok(PyRep::String("127.0.0.1:26000".into())))
        }

        ("warStatisticMgr", "Bind") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("warpVectorMgr", "UseVector") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("warsInfoMgr", "GetWarsByOwners") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        _ => None,
    }
}
