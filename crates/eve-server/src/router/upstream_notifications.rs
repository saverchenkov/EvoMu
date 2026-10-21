//! Server-side emitters for the *upstream* notifications that the V24.01 client turns into its
//! own client-local events. Each helper names the client handler (from V24 bytecode) that unpacks
//! the payload, so the argument order below is the order that handler reads:
//!
//! | client-local event (never sent by the server) | upstream server notification (this file) |
//! |---|---|
//! | `OnModuleActivated` | `OnGodmaShipEffect` (`godma.StateManager.OnGodmaShipEffect` → `_NotifyOfEffectActivation`) |
//! | `OnModuleOnlineChange` | `OnModuleAttributeChanges` (`StateManager.OnModuleAttributeChanges` → `ApplyAttributeChange`, `attributeIsOnline`) |
//! | `OnTargets` | `OnMultiEvent` carrying `OnTarget` events (`godma.Godma.OnMultiEvent` → `OnMultiEvent_`) |
//! | `OnSuspectsAndCriminalsUpdate` | `OnSystemCriminalFlagUpdates` (`crimewatchSvc.OnSystemCriminalFlagUpdates`) |
//! | `OnSkillQueuePaused` | `OnSkillQueuePausedServer` (`skillQueueSvc.OnSkillQueuePausedServer`) |
//! | `OnSkillQueueChanged` | `OnNewSkillQueueSaved` (`skillQueueSvc.OnNewSkillQueueSaved`) |
//! | `OnColonyPinCountUpdated` | `OnMajorPlanetStateUpdate` (`planetSvc.OnMajorPlanetStateUpdate` → `_UpdateColonyNumberOfPins`) |
//! | `OnModuleUnfitted` | `OnItemChange` with an `ixFlag` change (`inventorysvc.InventorySvc.OnItemChange`) |

use crate::router::{NotificationTarget, PendingMachoNotification, ServerContext};
use eve_proto::PyRep;

/// `dogma/const.pyj`: `attributeIsOnline = 2`.
pub const ATTRIBUTE_IS_ONLINE: u32 = 2;
/// `carbon/common/lib/const.pyj`: `ixLocationID = 3`.
pub const IX_LOCATION_ID: i32 = 3;
/// `carbon/common/lib/const.pyj`: `ixFlag = 4`.
pub const IX_FLAG: i32 = 4;
/// `eve/common/lib/appConst.pyj`: `locationJunkyard = 10`.
pub const LOCATION_JUNKYARD: i64 = 10;
/// `inventorycommon/const.pyj`: `flagJunkyardTrashed = 147`.
pub const FLAG_JUNKYARD_TRASHED: i64 = 147;

/// One `('OnModuleAttributeChange', ownerID, itemKey, attributeID, time, newValue, oldValue)` entry.
/// `StateManager.OnModuleAttributeChanges` reads `itemKeyIdx, attributeIdx, newValueIdx = 2, 3, 5`
/// and calls `OnModuleAttributeChange_(*each[1:] + (0,))`.
#[derive(Debug, Clone)]
pub struct ModuleAttributeChange {
    pub owner_id: i64,
    pub item_id: i64,
    pub attribute_id: u32,
    pub new_value: PyRep,
    pub old_value: PyRep,
}

impl ServerContext {
    /// `OnModuleAttributeChanges(changes)`; see [`ModuleAttributeChange`].
    pub fn notify_module_attribute_changes(
        &self,
        char_id: i64,
        changes: &[ModuleAttributeChange],
    ) -> Vec<PendingMachoNotification> {
        let now = self.now_blueos_filetime_ticks();
        let list = changes
            .iter()
            .map(|c| {
                PyRep::Tuple(vec![
                    PyRep::String("OnModuleAttributeChange".into()),
                    PyRep::Integer(c.owner_id),
                    PyRep::Integer(c.item_id),
                    PyRep::Integer(c.attribute_id as i64),
                    PyRep::Integer(now),
                    c.new_value.clone(),
                    c.old_value.clone(),
                ])
            })
            .collect();
        self.notify(
            NotificationTarget::Character(char_id),
            "OnModuleAttributeChanges",
            "charid",
            vec![PyRep::List(list)],
        )
    }

    /// `OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`; the client re-inits the
    /// planet, updates colony presence when `numCommandCentersChanged`, and recounts pins
    /// (`_UpdateColonyNumberOfPins` scatters `OnColonyPinCountUpdated`).
    pub fn notify_major_planet_state_update(
        &self,
        char_id: i64,
        planet_id: i64,
        num_command_centers_changed: bool,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnMajorPlanetStateUpdate",
            "charid",
            vec![
                PyRep::Integer(planet_id),
                PyRep::Bool(num_command_centers_changed),
            ],
        )
    }

    pub fn notify_fleet_invite(&self, char_id: i64, inviter_id: i64, fleet_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFleetInvite", "charid", vec![PyRep::Integer(inviter_id), PyRep::Integer(fleet_id)])
    }
    pub fn notify_fleet_disbanded(&self, fleet_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetDisbanded", "fleetid", vec![PyRep::Integer(fleet_id)])
    }
    pub fn notify_fleet_join(&self, fleet_id: i64, char_id: i64, role: i64, wing_id: i64, squad_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetJoin", "fleetid", vec![PyRep::Integer(char_id), PyRep::Integer(role), PyRep::Integer(wing_id), PyRep::Integer(squad_id)])
    }
    pub fn notify_fleet_leave(&self, fleet_id: i64, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetLeave", "fleetid", vec![PyRep::Integer(char_id)])
    }
    pub fn notify_contact_logged_on(&self, char_id: i64, contact_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnContactLoggedOn", "charid", vec![PyRep::Integer(contact_id)])
    }
    pub fn notify_fleet_join_request(&self, fleet_id: i64, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetJoinRequest", "fleetid", vec![PyRep::Integer(char_id), PyRep::Integer(fleet_id)])
    }
    pub fn notify_fleet_join_reject(&self, char_id: i64, fleet_id: i64, reason: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFleetJoinReject", "charid", vec![PyRep::Integer(fleet_id), PyRep::String(reason.into())])
    }
    pub fn notify_fleet_join_rejected(&self, char_id: i64, fleet_id: i64, reason: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFleetJoinRejected", "charid", vec![PyRep::Integer(fleet_id), PyRep::String(reason.into())])
    }
    pub fn notify_fleet_member_changed(&self, fleet_id: i64, char_id: i64, changes: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetMemberChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(char_id), changes])
    }
    pub fn notify_fleet_motd_changed(&self, fleet_id: i64, motd: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetMotdChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::String(motd.into())])
    }
    pub fn notify_change_fleet_boss(&self, fleet_id: i64, new_boss_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnChangeFleetBoss", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(new_boss_id)])
    }
    pub fn notify_fleet_options_changed(&self, fleet_id: i64, options: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetOptionsChanged", "fleetid", vec![PyRep::Integer(fleet_id), options])
    }
    pub fn notify_fleet_setup_changed(&self, fleet_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSetupChanged", "fleetid", vec![PyRep::Integer(fleet_id)])
    }
    pub fn notify_fleet_wing_added(&self, fleet_id: i64, wing_id: i64, name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingAdded", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(wing_id), PyRep::String(name.into())])
    }
    pub fn notify_fleet_wing_deleted(&self, fleet_id: i64, wing_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingDeleted", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(wing_id)])
    }
    pub fn notify_fleet_wing_name_changed(&self, fleet_id: i64, wing_id: i64, name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingNameChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(wing_id), PyRep::String(name.into())])
    }
    pub fn notify_fleet_squad_added(&self, fleet_id: i64, wing_id: i64, squad_id: i64, name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadAdded", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(wing_id), PyRep::Integer(squad_id), PyRep::String(name.into())])
    }
    pub fn notify_fleet_squad_deleted(&self, fleet_id: i64, squad_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadDeleted", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(squad_id)])
    }
    pub fn notify_fleet_squad_name_changed(&self, fleet_id: i64, squad_id: i64, name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadNameChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(squad_id), PyRep::String(name.into())])
    }
    pub fn notify_fleet_finder_advert_changed(&self, fleet_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetFinderAdvertChanged", "fleetid", vec![PyRep::Integer(fleet_id)])
    }
    pub fn notify_fleet_state_change(&self, fleet_id: i64, state: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetStateChange", "fleetid", vec![PyRep::Integer(fleet_id), state])
    }
    pub fn notify_fleet_jump_beacon_module_change(&self, char_id: i64, sys_id: u32, active: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFleetJumpBeaconModuleChange", "charid", vec![PyRep::Integer(sys_id as i64), PyRep::Bool(active)])
    }
    pub fn notify_channel_joined(&self, char_id: i64, channel_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnChannelJoined", "charid", vec![PyRep::Integer(channel_id), PyRep::Integer(char_id)])
    }
    pub fn notify_channel_left(&self, char_id: i64, channel_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnChannelLeft", "charid", vec![PyRep::Integer(channel_id), PyRep::Integer(char_id)])
    }
    pub fn notify_channel_deleted(&self, char_id: i64, channel_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnChannelDeleted", "charid", vec![PyRep::Integer(channel_id)])
    }
    pub fn notify_chat_message_sent(&self, char_id: i64, channel_id: i64, msg: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnClientEvent_ChatMessageSent", "charid", vec![PyRep::Integer(channel_id), PyRep::Integer(char_id), PyRep::String(msg.into())])
    }
    pub fn notify_mail_sent(&self, char_id: i64, mail_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMailSent", "charid", vec![PyRep::Integer(mail_id)])
    }
    pub fn notify_new_mail_received(&self, char_id: i64, mail_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNewMailReceived", "charid", vec![PyRep::Integer(mail_id)])
    }
    pub fn notify_mail_deleted(&self, char_id: i64, mail_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMailDeleted", "charid", vec![PyRep::Integer(mail_id)])
    }
    pub fn notify_mail_trashed(&self, char_id: i64, mail_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMailTrashed", "charid", vec![PyRep::Integer(mail_id)])
    }
    pub fn notify_mail_restored(&self, char_id: i64, mail_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMailRestored", "charid", vec![PyRep::Integer(mail_id)])
    }
    pub fn notify_mail_counters_update(&self, char_id: i64, unread: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMailCountersUpdate", "charid", vec![PyRep::Integer(unread)])
    }
    pub fn notify_calendar_event(&self, char_id: i64, event_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNewCalendarEvent", "charid", vec![PyRep::Integer(event_id)])
    }
    pub fn notify_notification_deleted(&self, char_id: i64, notif_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNotificationDeleted", "charid", vec![PyRep::Integer(notif_id)])
    }
    pub fn notify_corporation_changed(&self, corp_id: i64, data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), data])
    }
    pub fn notify_corporation_member_changed(&self, corp_id: i64, char_id: i64, roles: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMemberChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(char_id), PyRep::Integer(roles)])
    }
    pub fn notify_corporation_application_changed(&self, char_id: i64, corp_id: i64, status: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCorporationApplicationChanged", "charid", vec![PyRep::Integer(corp_id), PyRep::String(status.into())])
    }
    pub fn notify_corporation_recruitment_ad_changed(&self, corp_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorporationRecruitmentAdChanged", "corpid", vec![PyRep::Integer(corp_id)])
    }
    pub fn notify_corporation_welcome_mail_changed(&self, corp_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorporationWelcomeMailChanged", "corpid", vec![PyRep::Integer(corp_id)])
    }
    pub fn notify_corporation_medal_added(&self, corp_id: i64, medal_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(medal_id)])
    }
    pub fn notify_corp_logo_ready(&self, corp_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorpLogoReady", "corpid", vec![PyRep::Integer(corp_id)])
    }
    pub fn notify_corp_aggression_settings_change(&self, corp_id: i64, settings: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorpAggressionSettingsChange", "corpid", vec![PyRep::Integer(corp_id), settings])
    }
    pub fn notify_corp_allowed_enlistment_factions_set(&self, corp_id: i64, factions: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorpAllowedEnlistmentFactionsSet", "corpid", vec![PyRep::Integer(corp_id), factions])
    }
    pub fn notify_alliance_changed(&self, alliance_id: i64, data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Alliance(alliance_id), "OnAllianceChanged", "allianceid", vec![PyRep::Integer(alliance_id), data])
    }
    pub fn notify_alliance_member_changed(&self, alliance_id: i64, corp_id: i64, status: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Alliance(alliance_id), "OnAllianceMemberChanged", "allianceid", vec![PyRep::Integer(alliance_id), PyRep::Integer(corp_id), PyRep::String(status.into())])
    }
    pub fn notify_alliance_application_changed(&self, corp_id: i64, alliance_id: i64, status: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnAllianceApplicationChanged", "corpid", vec![PyRep::Integer(alliance_id), PyRep::String(status.into())])
    }
    pub fn notify_alliance_relationship_changed(&self, alliance_id: i64, other_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Alliance(alliance_id), "OnAllianceRelationshipChanged", "allianceid", vec![PyRep::Integer(alliance_id), PyRep::Integer(other_id), PyRep::Real(standing)])
    }
    pub fn notify_contact_change(&self, char_id: i64, contact_id: i64, relationship: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnContactChange", "charid", vec![PyRep::Integer(contact_id), PyRep::Real(relationship)])
    }
    pub fn notify_contact_no_longer_contact(&self, char_id: i64, contact_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnContactNoLongerContact", "charid", vec![PyRep::Integer(contact_id)])
    }
    pub fn notify_personal_contacts_added(&self, char_id: i64, contacts: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPersonalContactsAdded", "charid", vec![contacts])
    }
    pub fn notify_personal_contacts_updated(&self, char_id: i64, contacts: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPersonalContactsUpdated", "charid", vec![contacts])
    }
    pub fn notify_personal_contacts_deleted(&self, char_id: i64, contact_ids: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPersonalContactsDeleted", "charid", vec![contact_ids])
    }
    pub fn notify_block_contacts(&self, char_id: i64, contact_ids: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBlockContacts", "charid", vec![contact_ids])
    }
    pub fn notify_unblock_contact(&self, char_id: i64, contact_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUnblockContact", "charid", vec![PyRep::Integer(contact_id)])
    }
    pub fn notify_unblock_contacts(&self, char_id: i64, contact_ids: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUnblockContacts", "charid", vec![contact_ids])
    }
    pub fn notify_standing_set(&self, owner_id: i64, target_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(owner_id), "OnStandingSet", "charid", vec![PyRep::Integer(owner_id), PyRep::Integer(target_id), PyRep::Real(standing)])
    }
    pub fn notify_standings_modified(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnStandingsModified", "charid", vec![])
    }
    pub fn notify_set_corp_standing(&self, corp_id: i64, target_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnSetCorpStanding", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(target_id), PyRep::Real(standing)])
    }
    pub fn notify_bounty_placed(&self, char_id: i64, target_id: i64, amount: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBountyPlaced", "charid", vec![PyRep::Integer(target_id), PyRep::Real(amount)])
    }
    pub fn notify_bounty_added_to_payout(&self, char_id: i64, amount: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBountyAddedToPayout", "charid", vec![PyRep::Real(amount)])
    }
    pub fn notify_trade_initiate(&self, char_id: i64, other_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeInitiate", "charid", vec![PyRep::Integer(other_id)])
    }
    pub fn notify_trade_offer(&self, char_id: i64, other_id: i64, offer: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeOffer", "charid", vec![PyRep::Integer(other_id), offer])
    }
    pub fn notify_trade_money_offer(&self, char_id: i64, other_id: i64, isk: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeMoneyOffer", "charid", vec![PyRep::Integer(other_id), PyRep::Real(isk)])
    }
    pub fn notify_trade_offer_reset(&self, char_id: i64, other_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeOfferReset", "charid", vec![PyRep::Integer(other_id)])
    }
    pub fn notify_trade_complete(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeComplete", "charid", vec![])
    }
    pub fn notify_trade_cancel(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTradeCancel", "charid", vec![])
    }
    pub fn notify_raffle_created(&self, char_id: i64, raffle_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRaffleCreatedServer", "charid", vec![PyRep::Integer(raffle_id)])
    }
    pub fn notify_raffle_finished(&self, char_id: i64, raffle_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRaffleFinishedServer", "charid", vec![PyRep::Integer(raffle_id)])
    }
    pub fn notify_raffle_updated(&self, char_id: i64, raffle_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRaffleUpdatedServer", "charid", vec![PyRep::Integer(raffle_id)])
    }
    pub fn notify_warp_started(&self, item_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(item_id), "OnWarpStarted", "itemid", vec![PyRep::Integer(item_id)])
    }
    pub fn notify_warp_finished(&self, item_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(item_id), "OnWarpFinished", "itemid", vec![PyRep::Integer(item_id)])
    }
    pub fn notify_autopilot_updated(&self, char_id: i64, route: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnAutopilotUpdated", "charid", vec![route])
    }
    pub fn notify_char_now_in_station(&self, station_id: i64, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Station(station_id), "OnCharNowInStation", "stationid", vec![PyRep::Integer(char_id)])
    }
    pub fn notify_char_no_longer_in_station(&self, station_id: i64, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Station(station_id), "OnCharNoLongerInStation", "stationid", vec![PyRep::Integer(char_id)])
    }
    pub fn notify_capacitor_change(&self, char_id: i64, val: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCapacitorChange", "charid", vec![PyRep::Real(val)])
    }
    pub fn notify_ore_mined(&self, char_id: i64, type_id: u32, volume: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnOreMined", "charid", vec![PyRep::Integer(type_id as i64), PyRep::Real(volume)])
    }
    pub fn notify_drone_state_change(&self, char_id: i64, drone_id: i64, state: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDroneStateChange", "charid", vec![PyRep::Integer(drone_id), PyRep::String(state.into())])
    }
    pub fn notify_drone_control_lost(&self, char_id: i64, drone_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDroneControlLost", "charid", vec![PyRep::Integer(drone_id)])
    }
    pub fn notify_bridge_mode_change(&self, item_id: i64, active: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(item_id), "OnBridgeModeChange", "itemid", vec![PyRep::Integer(item_id), PyRep::Bool(active)])
    }
    pub fn notify_crimewatch_engagement_updated(&self, char_id: i64, other_id: i64, active: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCrimewatchEngagementUpdated", "charid", vec![PyRep::Integer(other_id), PyRep::Bool(active)])
    }
    pub fn notify_wreck_loot_all(&self, char_id: i64, wreck_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWreckLootAll", "charid", vec![PyRep::Integer(wreck_id)])
    }
    pub fn notify_sim_clock_rebase(&self, sys_id: u32, clock: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "DoSimClockRebase", "solarsystemid", vec![PyRep::Integer(clock)])
    }
    pub fn notify_system_scan_started(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSystemScanStarted", "charid", vec![])
    }
    pub fn notify_system_scan_done(&self, char_id: i64, results: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSystemScanDone", "charid", vec![results])
    }
    pub fn notify_system_scan_stopped(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSystemScanStopped", "charid", vec![])
    }
    pub fn notify_new_probe(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNewProbe", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_remove_probe(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRemoveProbe", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_probes_idle(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnProbesIdle", "charid", vec![])
    }
    pub fn notify_directional_scan_complete(&self, char_id: i64, results: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDirectionalScanComplete", "charid", vec![results])
    }
    pub fn notify_ship_scan_completed(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShipScanCompleted", "charid", vec![PyRep::Integer(ship_id)])
    }
    pub fn notify_cargo_scan_complete(&self, char_id: i64, ship_id: i64, items: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCargoScanComplete", "charid", vec![PyRep::Integer(ship_id), items])
    }
    pub fn notify_moon_scan_complete(&self, char_id: i64, moon_id: i64, mats: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMoonScanComplete", "charid", vec![PyRep::Integer(moon_id), mats])
    }
    pub fn notify_new_moon_probe(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNewMoonProbe", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_remove_moon_probe(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRemoveMoonProbe", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_perfect_scan_result_reached(&self, char_id: i64, sig_id: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnClientEvent_PerfectScanResultReached", "charid", vec![PyRep::String(sig_id.into())])
    }
    pub fn notify_signal_tracker_anomaly_update(&self, sys_id: u32, anomaly_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSignalTrackerAnomalyUpdate", "solarsystemid", vec![PyRep::Integer(anomaly_id)])
    }
    pub fn notify_signal_tracker_signature_update(&self, sys_id: u32, sig_id: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSignalTrackerSignatureUpdate", "solarsystemid", vec![PyRep::String(sig_id.into())])
    }
    pub fn notify_signal_tracker_structure_update(&self, sys_id: u32, structure_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSignalTrackerStructureUpdate", "solarsystemid", vec![PyRep::Integer(structure_id)])
    }
    pub fn notify_free_skill_points_changed(&self, char_id: i64, free_sp: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFreeSkillPointsChanged", "charid", vec![PyRep::Integer(free_sp)])
    }
    pub fn notify_booster_updated(&self, char_id: i64, booster_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBoosterUpdated", "charid", vec![PyRep::Integer(booster_id)])
    }
    pub fn notify_implants_changed(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnImplantsChanged", "charid", vec![])
    }
    pub fn notify_skin_license_activated(&self, char_id: i64, license_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkinLicenseActivated", "charid", vec![PyRep::Integer(license_id)])
    }
    pub fn notify_skin_license_removed(&self, char_id: i64, license_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkinLicenseRemoved", "charid", vec![PyRep::Integer(license_id)])
    }
    pub fn notify_current_ship_skin_change(&self, char_id: i64, skin_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCurrentShipSkinChange", "charid", vec![PyRep::Integer(skin_id)])
    }
    pub fn notify_industry_job(&self, char_id: i64, job_id: i64, status: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIndustryJob", "charid", vec![PyRep::Integer(job_id), PyRep::String(status.into())])
    }
    pub fn notify_reprocessing_materials_changed(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnReprocessingMaterialsChanged", "charid", vec![])
    }
    pub fn notify_sovereignty_changed(&self, sys_id: u32, data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSovereigntyChanged", "solarsystemid", vec![PyRep::Integer(sys_id as i64), data])
    }
    pub fn notify_sov_hub_hacked(&self, sys_id: u32, hub_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSovHubHacked", "solarsystemid", vec![PyRep::Integer(hub_id)])
    }
    pub fn notify_skyhook_can_be_hacked(&self, sys_id: u32, skyhook_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSkyhookCanBeHacked", "solarsystemid", vec![PyRep::Integer(skyhook_id)])
    }
    pub fn notify_solar_system_dev_index_changed(&self, sys_id: u32, index_type: i64, level: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSolarSystemDevIndexChanged", "solarsystemid", vec![PyRep::Integer(sys_id as i64), PyRep::Integer(index_type), PyRep::Real(level)])
    }
    pub fn notify_solar_system_sov_structures_updated(&self, sys_id: u32) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSolarSystemSovStructuresUpdated", "solarsystemid", vec![PyRep::Integer(sys_id as i64)])
    }
    pub fn notify_solarsystem_advantage_state_updated(&self, sys_id: u32, advantage: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSolarsystemAdvantageStateUpdated_Local", "solarsystemid", vec![PyRep::Integer(sys_id as i64), PyRep::Real(advantage)])
    }
    pub fn notify_solarsystem_interference_changed(&self, sys_id: u32, level: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSolarsystemInterferenceChanged", "solarsystemid", vec![PyRep::Integer(sys_id as i64), PyRep::Real(level)])
    }
    pub fn notify_enlistment_state_updated(&self, char_id: i64, faction_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnEnlistmentStateUpdated", "charid", vec![PyRep::Integer(faction_id)])
    }
    pub fn notify_insurgency_campaign_started(&self, loc_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(loc_id as u32), "OnInsurgencyCampaignStartedForLocation", "solarsystemid", vec![PyRep::Integer(loc_id)])
    }
    pub fn notify_insurgency_campaign_updated(&self, loc_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(loc_id as u32), "OnInsurgencyCampaignUpdatedForLocation", "solarsystemid", vec![PyRep::Integer(loc_id)])
    }
    pub fn notify_war_changed(&self, war_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnWarChanged", "all", vec![PyRep::Integer(war_id)])
    }
    pub fn notify_structure_service_changed(&self, structure_id: i64, service_id: i64, online: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(structure_id), "OnStructureServiceChanged", "itemid", vec![PyRep::Integer(structure_id), PyRep::Integer(service_id), PyRep::Bool(online)])
    }
    pub fn notify_bookmark_created(&self, char_id: i64, bm_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBookmarkCreated", "charid", vec![PyRep::Integer(bm_id)])
    }
    pub fn notify_refresh_bookmarks(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRefreshBookmarks", "charid", vec![])
    }
    pub fn notify_shared_bookmarks_folder_updated(&self, folder_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnSharedBookmarksFolderUpdated", "all", vec![PyRep::Integer(folder_id)])
    }
    pub fn notify_shared_bookmarks_folder_deleted(&self, folder_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnSharedBookmarksFolderDeleted", "all", vec![PyRep::Integer(folder_id)])
    }
    pub fn notify_hacking_start(&self, char_id: i64, dungeon_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnHackingStart", "charid", vec![PyRep::Integer(dungeon_id)])
    }
    pub fn notify_hacking_turn_complete(&self, char_id: i64, state: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnHackingTurnComplete", "charid", vec![state])
    }
    pub fn notify_hacking_virus_changed(&self, char_id: i64, data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnHackingVirusChanged", "charid", vec![data])
    }
    pub fn notify_stance_active(&self, item_id: i64, stance_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(item_id), "OnStanceActive", "itemid", vec![PyRep::Integer(item_id), PyRep::Integer(stance_id)])
    }
    pub fn notify_system_wide_effect_add(&self, sys_id: u32, effect_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSystemWideEffectAdd", "solarsystemid", vec![PyRep::Integer(sys_id as i64), PyRep::Integer(effect_id)])
    }
    pub fn notify_system_wide_effect_remove(&self, sys_id: u32, effect_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSystemWideEffectRemove", "solarsystemid", vec![PyRep::Integer(sys_id as i64), PyRep::Integer(effect_id)])
    }
    pub fn notify_escalating_path_message(&self, char_id: i64, msg: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnEscalatingPathMessage", "charid", vec![PyRep::String(msg.into())])
    }
    pub fn notify_petition_created(&self, char_id: i64, petition_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPetitionCreated", "charid", vec![PyRep::Integer(petition_id)])
    }
    pub fn notify_window_opened(&self, char_id: i64, wnd_name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWindowOpened", "charid", vec![PyRep::String(wnd_name.into())])
    }

    // Ship & Module notifications
    pub fn notify_targets(&self, char_id: i64, targets: Vec<PyRep>) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTargets", "charid", vec![PyRep::List(targets)])
    }
    pub fn notify_slim_item_updated(&self, sys_id: u32, item_id: i64, slim_item: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnSlimItemUpdated", "solarsystemid", vec![PyRep::Integer(item_id), slim_item])
    }
    pub fn notify_drone_state_change2(&self, char_id: i64, drone_id: i64, activity: i64, target_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDroneStateChange2", "charid", vec![PyRep::Integer(drone_id), PyRep::Integer(activity), PyRep::Integer(target_id)])
    }
    pub fn notify_weapons_timer_update(&self, char_id: i64, duration_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWeaponsTimerUpdate", "charid", vec![PyRep::Integer(duration_ms)])
    }
    pub fn notify_damage_state_changed(&self, sys_id: u32, item_id: i64, damage_state: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "OnDamageStateChanged", "solarsystemid", vec![PyRep::Integer(item_id), damage_state])
    }
    pub fn notify_module_attribute_change(&self, char_id: i64, item_id: i64, attr_id: i64, val: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnModuleAttributeChange", "charid", vec![PyRep::Integer(item_id), PyRep::Integer(attr_id), PyRep::Real(val)])
    }
    pub fn notify_module_activated(&self, char_id: i64, module_id: i64, effect_id: i64, target_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnModuleActivated", "charid", vec![PyRep::Integer(module_id), PyRep::Integer(effect_id), PyRep::Integer(target_id)])
    }
    pub fn notify_module_repaired(&self, char_id: i64, module_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnModuleRepaired", "charid", vec![PyRep::Integer(module_id)])
    }
    pub fn notify_module_unfitted(&self, char_id: i64, module_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnModuleUnfitted", "charid", vec![PyRep::Integer(module_id), PyRep::Integer(ship_id)])
    }
    pub fn notify_module_online_change(&self, char_id: i64, module_id: i64, is_online: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnModuleOnlineChange", "charid", vec![PyRep::Integer(module_id), PyRep::Bool(is_online)])
    }
    pub fn notify_dogma_item_change(&self, char_id: i64, item_id: i64, change_type: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDogmaItemChange", "charid", vec![PyRep::Integer(item_id), PyRep::Integer(change_type)])
    }
    pub fn notify_godma_item_change(&self, char_id: i64, item_id: i64, item_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnGodmaItemChange", "charid", vec![PyRep::Integer(item_id), item_data])
    }
    pub fn notify_godma_prime_item(&self, char_id: i64, item_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnGodmaPrimeItem", "charid", vec![PyRep::Integer(item_id)])
    }
    pub fn notify_godma_effect_info(&self, char_id: i64, effect_id: i64, info: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnGodmaEffectInfo", "charid", vec![PyRep::Integer(effect_id), info])
    }
    pub fn notify_target_lock_started(&self, char_id: i64, target_id: i64, delay_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTargetLockStarted", "charid", vec![PyRep::Integer(target_id), PyRep::Integer(delay_ms)])
    }
    pub fn notify_fail_lock_target(&self, char_id: i64, target_id: i64, reason: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnFailLockTarget", "charid", vec![PyRep::Integer(target_id), PyRep::String(reason.into())])
    }
    pub fn notify_target_obo(&self, char_id: i64, target_id: i64, obo_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTargetOBO", "charid", vec![PyRep::Integer(target_id), obo_data])
    }
    pub fn notify_process_target_changed(&self, char_id: i64, target_id: i64, change_type: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "ProcessTargetChanged", "charid", vec![PyRep::Integer(target_id), PyRep::Integer(change_type)])
    }
    pub fn notify_ship_death(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShipDeath", "charid", vec![PyRep::Integer(ship_id)])
    }
    pub fn notify_ship_insured(&self, char_id: i64, ship_id: i64, payout: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShipInsured", "charid", vec![PyRep::Integer(ship_id), PyRep::Real(payout)])
    }

    // Session & State notifications
    pub fn notify_do_session_changing(&self, char_id: i64, session_id: i64, reason: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "DoSessionChanging", "charid", vec![PyRep::Integer(session_id), PyRep::String(reason.into())])
    }
    pub fn notify_character_session_changed(&self, char_id: i64, session_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCharacterSessionChanged", "charid", vec![session_data])
    }
    pub fn notify_curr_session_space_entered_first_time(&self, char_id: i64, sys_id: u32) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCurrSessionSpaceEnteredFirstTime", "charid", vec![PyRep::Integer(sys_id as i64)])
    }
    pub fn notify_session_end(&self, char_id: i64, session_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSessionEnd", "charid", vec![PyRep::Integer(session_id)])
    }
    pub fn notify_session_mutated(&self, char_id: i64, session_id: i64, changes: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSessionMutated", "charid", vec![PyRep::Integer(session_id), changes])
    }
    pub fn notify_session_reset(&self, char_id: i64, session_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSessionReset", "charid", vec![PyRep::Integer(session_id)])
    }
    pub fn notify_process_session_change(&self, char_id: i64, changes: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "ProcessSessionChange", "charid", vec![changes])
    }
    pub fn notify_process_session_reset(&self, char_id: i64, session_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "ProcessSessionReset", "charid", vec![PyRep::Integer(session_id)])
    }
    pub fn notify_time_reset(&self, char_id: i64, timestamp: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTimeReset", "charid", vec![PyRep::Integer(timestamp)])
    }
    pub fn notify_server_skills_changed(&self, char_id: i64, skills: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerSkillsChanged", "charid", vec![skills])
    }
    pub fn notify_server_skills_removed(&self, char_id: i64, skill_ids: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerSkillsRemoved", "charid", vec![skill_ids])
    }
    pub fn notify_server_implants_changed(&self, char_id: i64, implants: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerImplantsChanged", "charid", vec![implants])
    }
    pub fn notify_server_boosters_changed(&self, char_id: i64, boosters: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerBoostersChanged", "charid", vec![boosters])
    }
    pub fn notify_server_brain_updated(&self, char_id: i64, brain_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerBrainUpdated", "charid", vec![brain_data])
    }
    pub fn notify_skills_changed(&self, char_id: i64, skills_diff: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkillsChanged", "charid", vec![skills_diff])
    }
    pub fn notify_skill_queue_changed_with_data(&self, char_id: i64, queue_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkillQueueChanged", "charid", vec![queue_data])
    }
    pub fn notify_skill_queue_refreshed(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSkillQueueRefreshed", "charid", vec![])
    }

    // Fleet & Social notifications
    pub fn notify_contact_logged_off(&self, char_id: i64, contact_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnContactLoggedOff", "charid", vec![PyRep::Integer(contact_id)])
    }
    pub fn notify_set_player_standing(&self, char_id: i64, target_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSetPlayerStanding", "charid", vec![PyRep::Integer(target_id), PyRep::Real(standing)])
    }
    pub fn notify_set_alliance_standing(&self, alliance_id: i64, target_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Alliance(alliance_id), "OnSetAllianceStanding", "allianceid", vec![PyRep::Integer(target_id), PyRep::Real(standing)])
    }
    pub fn notify_title_changed(&self, char_id: i64, title_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTitleChanged", "charid", vec![PyRep::Integer(title_id)])
    }
    pub fn notify_fleet_boosted(&self, fleet_id: i64, booster_id: i64, boost_type: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetBoosted", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(booster_id), PyRep::Integer(boost_type)])
    }
    pub fn notify_fleet_loot_event(&self, fleet_id: i64, char_id: i64, item_id: i64, quantity: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetLootEvent", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(char_id), PyRep::Integer(item_id), PyRep::Integer(quantity)])
    }
    pub fn notify_fleet_member_changing(&self, fleet_id: i64, char_id: i64, role: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetMemberChanging", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(char_id), PyRep::Integer(role)])
    }
    pub fn notify_fleet_formation_wnd_changed(&self, fleet_id: i64, formation_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetFormationWndChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(formation_id)])
    }
    pub fn notify_fleet_favorite_added(&self, fleet_id: i64, favorite_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetFavoriteAdded", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(favorite_id)])
    }
    pub fn notify_fleet_favorite_removed(&self, fleet_id: i64, favorite_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetFavoriteRemoved", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(favorite_id)])
    }
    pub fn notify_npc_standing_change(&self, char_id: i64, faction_id: i64, standing: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNPCStandingChange", "charid", vec![PyRep::Integer(faction_id), PyRep::Real(standing)])
    }
    pub fn notify_combat_timers_updated(&self, char_id: i64, timers: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCombatTimersUpdated", "charid", vec![timers])
    }
    pub fn notify_criminal_timer_update(&self, char_id: i64, remaining_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCriminalTimerUpdate", "charid", vec![PyRep::Integer(remaining_ms)])
    }
    pub fn notify_disapproval_timer_update(&self, char_id: i64, remaining_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDisapprovalTimerUpdate", "charid", vec![PyRep::Integer(remaining_ms)])
    }
    pub fn notify_npc_timer_update(&self, char_id: i64, remaining_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnNpcTimerUpdate", "charid", vec![PyRep::Integer(remaining_ms)])
    }
    pub fn notify_pvp_timer_update(&self, char_id: i64, remaining_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPvpTimerUpdate", "charid", vec![PyRep::Integer(remaining_ms)])
    }
    pub fn notify_client_contact_change(&self, char_id: i64, contact_id: i64, status: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnClientContactChange", "charid", vec![PyRep::Integer(contact_id), PyRep::Integer(status)])
    }
    pub fn notify_organization_contacts_updated(&self, corp_id: i64, contacts: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnOrganizationContactsUpdated", "corpid", vec![contacts])
    }

    // Market & Wallet notifications
    pub fn notify_bill_received(&self, char_id: i64, bill_id: i64, amount: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnBillReceived", "charid", vec![PyRep::Integer(bill_id), PyRep::Real(amount)])
    }
    pub fn notify_corp_wallet_bills_changed(&self, corp_id: i64, bill_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorpWalletBillsChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(bill_id)])
    }
    pub fn notify_corp_account_changed_client(&self, corp_id: i64, account_key: i64, balance: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnCorpAccountChangedClient", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(account_key), PyRep::Real(balance)])
    }
    pub fn notify_personal_account_changed_client(&self, char_id: i64, balance: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnPersonalAccountChangedClient", "charid", vec![PyRep::Integer(char_id), PyRep::Real(balance)])
    }
    pub fn notify_tokens_redeemed(&self, char_id: i64, token_ids: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTokensRedeemed", "charid", vec![token_ids])
    }
    pub fn notify_redeeming_tokens_updated(&self, char_id: i64, tokens: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRedeemingTokensUpdated", "charid", vec![tokens])
    }
    pub fn notify_character_lp_updated_in_wallet(&self, char_id: i64, corp_id: i64, lp: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCharacterLpUpdatedInWallet", "charid", vec![PyRep::Integer(corp_id), PyRep::Integer(lp)])
    }
    pub fn notify_redeeming_queue_updated(&self, char_id: i64, queue_items: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRedeemingQueueUpdated", "charid", vec![queue_items])
    }
    pub fn notify_redeem_wnd_opened(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnRedeemWndOpened", "charid", vec![])
    }

    // Map & Navigation notifications
    pub fn notify_map_mode_change_done(&self, char_id: i64, mode_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMapModeChangeDone", "charid", vec![PyRep::Integer(mode_id)])
    }
    pub fn notify_warp_decelerate(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWarpDecelerate", "charid", vec![PyRep::Integer(ship_id)])
    }
    pub fn notify_warp_arrived(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWarpArrived", "charid", vec![PyRep::Integer(ship_id)])
    }
    pub fn notify_warp_active(&self, char_id: i64, ship_id: i64, active: bool) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWarpActive", "charid", vec![PyRep::Integer(ship_id), PyRep::Bool(active)])
    }
    pub fn notify_warp_started2(&self, char_id: i64, ship_id: i64, dest_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWarpStarted2", "charid", vec![PyRep::Integer(ship_id), PyRep::Integer(dest_id)])
    }
    pub fn notify_travel_state_changed(&self, char_id: i64, state_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnTravelStateChanged", "charid", vec![PyRep::Integer(state_id)])
    }
    pub fn notify_undocking_started(&self, char_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUndockingStarted", "charid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_undocking_completed(&self, char_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUndockingCompleted", "charid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_undocking_aborted(&self, char_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUndockingAborted", "charid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_docking_accepted(&self, char_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDockingAccepted", "charid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_docking_finished(&self, char_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDockingFinished", "charid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_docking_progress_changed(&self, char_id: i64, progress: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDockingProgressChanged", "charid", vec![PyRep::Real(progress)])
    }
    pub fn notify_docking_request_was_denied(&self, char_id: i64, station_id: i64, reason: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDockingRequestWasDenied", "charid", vec![PyRep::Integer(station_id), PyRep::String(reason.into())])
    }
    pub fn notify_jump_cloak_cancelled(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnJumpCloakCancelled", "charid", vec![])
    }
    pub fn notify_jump_cloak_updated(&self, char_id: i64, remaining_ms: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnJumpCloakUpdated", "charid", vec![PyRep::Integer(remaining_ms)])
    }
    pub fn notify_autopilot_jump(&self, char_id: i64, next_sys_id: u32) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnAutoPilotJump", "charid", vec![PyRep::Integer(next_sys_id as i64)])
    }
    pub fn notify_clone_jump_update(&self, char_id: i64, target_station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnCloneJumpUpdate", "charid", vec![PyRep::Integer(target_station_id)])
    }
    pub fn notify_ship_clone_jump_update(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShipCloneJumpUpdate", "charid", vec![PyRep::Integer(ship_id)])
    }

    // Industry & Science notifications
    pub fn notify_industry_job_client(&self, char_id: i64, job_id: i64, status: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIndustryJobClient", "charid", vec![PyRep::Integer(job_id), PyRep::String(status.into())])
    }
    pub fn notify_industry_materials(&self, char_id: i64, job_id: i64, materials: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIndustryMaterials", "charid", vec![PyRep::Integer(job_id), materials])
    }
    pub fn notify_industry_slots_updated(&self, char_id: i64, facility_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIndustrySlotsUpdated", "charid", vec![PyRep::Integer(facility_id)])
    }
    pub fn notify_under_construction_updated(&self, char_id: i64, item_id: i64, progress: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUnderConstructionUpdated", "charid", vec![PyRep::Integer(item_id), PyRep::Real(progress)])
    }
    pub fn notify_selected_industry_job_changed(&self, char_id: i64, job_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSelectedIndustryJobChanged", "charid", vec![PyRep::Integer(job_id)])
    }
    pub fn notify_industry_material_value_setting_changed(&self, char_id: i64, setting_val: f64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIndustryMaterialValueSettingChanged", "charid", vec![PyRep::Real(setting_val)])
    }

    // Extended server-emitted notifications
    pub fn notify_void_space_expiry(&self, char_id: i64, expiry_sec: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnAddExpiryDurationForVoidSpaceContent", "charid", vec![PyRep::Integer(expiry_sec)])
    }
    pub fn notify_void_space_relog(&self, char_id: i64, dest_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnVoidSpaceRelog", "charid", vec![PyRep::Integer(dest_id)])
    }
    pub fn notify_server_message(&self, char_id: i64, msg: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnServerMessage", "charid", vec![PyRep::String(msg.into())])
    }
    pub fn notify_signal_tracker_full_state(&self, char_id: i64, state: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnSignalTrackerFullState", "charid", vec![state])
    }
    pub fn notify_charge_being_loaded_to_module(&self, char_id: i64, module_id: i64, charge_type_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnChargeBeingLoadedToModule", "charid", vec![PyRep::Integer(module_id), PyRep::Integer(charge_type_id)])
    }
    pub fn notify_incoming_agent_message(&self, char_id: i64, agent_id: i64, msg: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnIncomingAgentMessage", "charid", vec![PyRep::Integer(agent_id), PyRep::String(msg.into())])
    }
    pub fn notify_items_changed(&self, char_id: i64, items: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnItemsChanged", "charid", vec![items])
    }
    pub fn notify_lp_change(&self, char_id: i64, corp_id: i64, lp: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnLPChange", "charid", vec![PyRep::Integer(corp_id), PyRep::Integer(lp)])
    }
    pub fn notify_office_rental_changed(&self, corp_id: i64, station_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Corporation(corp_id), "OnOfficeRentalChanged", "corpid", vec![PyRep::Integer(station_id)])
    }
    pub fn notify_probe_state_changed(&self, char_id: i64, probe_id: i64, state: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(probe_id), PyRep::Integer(state)])
    }
    pub fn notify_probe_warp_start(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnProbeWarpStart", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_probe_warp_end(&self, char_id: i64, probe_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnProbeWarpEnd", "charid", vec![PyRep::Integer(probe_id)])
    }
    pub fn notify_reload_calendar(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnReloadCalendar", "charid", vec![])
    }
    pub fn notify_scanner_info_removed(&self, char_id: i64, sig_id: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnScannerInfoRemoved", "charid", vec![PyRep::String(sig_id.into())])
    }
    pub fn notify_share_change(&self, char_id: i64, corp_id: i64, shares: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShareChange", "charid", vec![PyRep::Integer(corp_id), PyRep::Integer(shares)])
    }
    pub fn notify_weapon_banks_changed(&self, char_id: i64, ship_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWeaponBanksChanged", "charid", vec![PyRep::Integer(ship_id)])
    }
    pub fn notify_weapon_group_destroyed(&self, char_id: i64, group_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnWeaponGroupDestroyed", "charid", vec![PyRep::Integer(group_id)])
    }
    pub fn notify_sov_status_changed(&self, sys_id: u32, sov_data: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::SolarSystem(sys_id), "ProcessSovStatusChanged", "solarsystemid", vec![PyRep::Integer(sys_id as i64), sov_data])
    }
    pub fn notify_fleet_move(&self, fleet_id: i64, dest_sys_id: u32) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Fleet(fleet_id), "OnFleetMove", "fleetid", vec![PyRep::Integer(dest_sys_id as i64)])
    }
    pub fn notify_effect_message(&self, char_id: i64, msg: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnEffectMessage", "charid", vec![PyRep::String(msg.into())])
    }
    pub fn notify_godma_flush_location_profile(&self, char_id: i64, loc_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnGodmaFlushLocationProfile", "charid", vec![PyRep::Integer(loc_id)])
    }
    pub fn notify_dungeon_edit(&self, char_id: i64, dungeon_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnDungeonEdit", "charid", vec![PyRep::Integer(dungeon_id)])
    }
    pub fn notify_cfg_data_changed(&self, table_name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnCfgDataChanged", "all", vec![PyRep::String(table_name.into())])
    }
    pub fn notify_cluster_shutdown_initiated(&self, minutes_left: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnClusterShutdownInitiated", "all", vec![PyRep::Integer(minutes_left)])
    }
    pub fn notify_cluster_shutdown_cancelled(&self) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::All, "OnClusterShutdownCancelled", "all", vec![])
    }
    pub fn notify_update_service_mapping(&self, char_id: i64, service_name: &str) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnUpdateServiceMapping", "charid", vec![PyRep::String(service_name.into())])
    }
    pub fn notify_agent_mission_change(&self, char_id: i64, mission_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnAgentMissionChange", "charid", vec![PyRep::Integer(mission_id)])
    }
    pub fn notify_jump_clone_cache_invalidated(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnJumpCloneCacheInvalidated", "charid", vec![])
    }
    pub fn notify_multiple_item_change(&self, char_id: i64, changes: PyRep) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnMultipleItemChange", "charid", vec![changes])
    }
    pub fn notify_ship_jump_clone_cache_invalidated(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnShipJumpCloneCacheInvalidated", "charid", vec![])
    }
    pub fn notify_godma_flush_location(&self, char_id: i64, loc_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(NotificationTarget::Character(char_id), "OnGodmaFlushLocation", "charid", vec![PyRep::Integer(loc_id)])
    }
}
