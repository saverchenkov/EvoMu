//! Declarative GM / QA slash-command registry, role policy, single-pass
//! parser, and central command dispatcher (`#1193`).
//!
//! `all_remote_slash_commands()`, `remote_slash_commands_for_role()`,
//! `required_slash_role()`, `slash_command_usage()`,
//! `format_remote_command_list_reason()`, and `format_slash_help()` are all
//! derived directly from [`SLASH_COMMAND_REGISTRY`] so advertisement, role
//! enforcement, usage help, and execution routing stay 1-to-1 in lockstep.
//!
//! Track 1 cutover is complete: every row in [`SLASH_COMMAND_REGISTRY`] binds a
//! mandatory [`SlashHandler`] (`PENDING_SLASH_HANDLER_COUNT == 0`), and all
//! `slash.SlashCmd` / `publicQaToolsServer.SlashCmd` calls route centrally
//! through [`dispatch_slash_command_async`] without legacy fallback chains.

#![forbid(unsafe_code)]

use eve_machonet::service_roles;
use eve_proto::{BridgeRpcError, ClientError, PyRep, UserErrorArg, UserErrorKey};

const ROLE_QA_GM: u64 = service_roles::ROLE_QA | service_roles::ROLE_GM_MASK;
const ROLE_SPAWN_GM: u64 = service_roles::ROLE_SPAWN | service_roles::ROLE_GM_MASK;
pub(crate) const ROLE_HIGH_GM: u64 = service_roles::ROLE_GMH
    | service_roles::ROLE_GMS
    | service_roles::ROLE_ADMIN
    | service_roles::ROLE_ROLEADMIN;
pub(crate) const ROLE_STRUCTURE_GM: u64 =
    service_roles::ROLE_ADMIN | service_roles::ROLE_GMH | service_roles::ROLE_GML;
const ROLE_ANY_REMOTE: u64 = service_roles::ROLE_QA
    | service_roles::ROLE_SPAWN
    | service_roles::ROLE_ROLEADMIN
    | service_roles::ROLE_GM_MASK;
const ROLE_GM: u64 = service_roles::ROLE_GM_MASK;

/// Shrink-only count of [`SLASH_COMMAND_REGISTRY`] entries still pending migration (`0` after Track 1 Batch 4).
pub const PENDING_SLASH_HANDLER_COUNT: usize = 0;

/// Heap-allocated `Send` future returned by async slash-command handlers (`#1193`).
pub type SlashBoxFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<PyRep, BridgeRpcError>> + Send + 'a>>;

/// Synchronous slash-command handler function signature (`#1193`).
pub type SyncSlashHandler = fn(
    &crate::router::ServerContext,
    &eve_machonet::SessionState,
    &SlashInvocation<'_>,
) -> Result<PyRep, BridgeRpcError>;

/// Synchronous slash-command handler signature requiring mutable session state (`#1193`).
pub type SyncMutSlashHandler = fn(
    &crate::router::ServerContext,
    &mut eve_machonet::SessionState,
    &SlashInvocation<'_>,
) -> Result<PyRep, BridgeRpcError>;

/// Asynchronous slash-command handler function signature (`#1193`).
pub type AsyncSlashHandler = for<'a> fn(
    &'a crate::router::ServerContext,
    &'a mut eve_machonet::SessionState,
    &'a SlashInvocation<'a>,
) -> SlashBoxFuture<'a>;

/// Bound slash-command handler dispatch target (`#1193`).
#[derive(Clone, Copy)]
pub enum SlashHandler {
    /// Synchronous handler borrowing `&SessionState`.
    Sync(SyncSlashHandler),
    /// Synchronous handler mutably borrowing `&mut SessionState`.
    SyncMut(SyncMutSlashHandler),
    /// Asynchronous handler returning a `Send` boxed future.
    Async(AsyncSlashHandler),
}

/// Result returned by synchronous [`dispatch_slash_command`].
#[derive(Debug)]
pub enum SlashDispatchOutcome {
    /// Command was resolved and executed, or rejected (role gate, list query, or unknown command).
    Handled(Result<PyRep, BridgeRpcError>),
    /// Command binds an `Async` or `SyncMut` handler requiring [`dispatch_slash_command_async`].
    RequiresAsync,
}

/// Declarative entry in [`SLASH_COMMAND_REGISTRY`].
#[derive(Clone, Copy)]
pub struct SlashCommandEntry {
    /// Canonical lowercase name WITHOUT leading `'/'`.
    pub name: &'static str,
    /// Additional aliases WITHOUT leading `'/'` recognized on lookup/dispatch.
    pub aliases: &'static [&'static str],
    /// Whether `name` appears in advertised remote slash command lists (`ALL_REMOTE_SLASH_COMMANDS`).
    pub advertised: bool,
    /// Role bitmask checked via `ServerContext::has_gm_role_mask(session.role, min_role)`.
    pub min_role: u64,
    /// Summary usage line (always non-empty).
    pub usage: &'static str,
    /// Legacy `/help <cmd>` detailed usage line when present (`None` matches legacy "(no detailed usage entry)").
    pub detailed_usage: Option<&'static str>,
    /// Short summary description.
    pub description: &'static str,
    /// Whether command head lookup ignores ASCII case (`notify_emitters_character_services` parity).
    pub case_insensitive: bool,
    /// Bound command handler.
    pub handler: SlashHandler,
}

/// Parsed slash command invocation (`cmd.trim()` starting with `'/'`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashInvocation<'a> {
    /// Trimmed original input string.
    pub raw: &'a str,
    /// Exact-case command head without leading `'/'` (e.g. `"WHOAMI"` or `"giveisk"`).
    pub head_raw: &'a str,
    /// Lowercased command head without leading `'/'` (e.g. `"whoami"` or `"giveisk"`).
    pub head_lower: String,
    /// Trimmed arguments following the command head.
    pub rest: &'a str,
    /// Whether non-empty whitespace separated command head from following characters in raw input.
    pub had_whitespace_after_head: bool,
}

macro_rules! cmd {
    ($name:expr, $aliases:expr, $adv:expr, $role:expr, $usage:expr, $detail:expr, $desc:expr, $handler:expr) => {
        SlashCommandEntry {
            name: $name,
            aliases: $aliases,
            advertised: $adv,
            min_role: $role,
            usage: $usage,
            detailed_usage: $detail,
            description: $desc,
            case_insensitive: false,
            handler: $handler,
        }
    };
}

macro_rules! cmd_icase {
    ($name:expr, $aliases:expr, $adv:expr, $role:expr, $usage:expr, $detail:expr, $desc:expr, $handler:expr) => {
        SlashCommandEntry {
            name: $name,
            aliases: $aliases,
            advertised: $adv,
            min_role: $role,
            usage: $usage,
            detailed_usage: $detail,
            description: $desc,
            case_insensitive: true,
            handler: $handler,
        }
    };
}

/// Master declarative slash-command registry (`#1193`).
///
/// Advertised entries are ordered alphabetically to match `ALL_REMOTE_SLASH_COMMANDS`.
pub const SLASH_COMMAND_REGISTRY: &[SlashCommandEntry] = &[
    cmd_icase!("achievements", &[], true, ROLE_GM, "/achievements <subcommand>", None, "Grant or inspect character achievements", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_achievements_slash)),
    cmd_icase!("advancetime", &[], true, ROLE_GM, "/advancetime <seconds>", Some("/advancetime <seconds> - Advance simulation clock"), "Advance simulation clock", SlashHandler::Async(crate::services::notify_emitters_character_services::dispatch_advancetime_slash)),
    cmd!("autotidi", &[], true, ROLE_GM, "/autotidi <threshold|off>", None, "Configure automatic Time Dilation threshold", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_autotidi_slash)),
    cmd!("backintime", &[], true, ROLE_GM, "/backintime [me]", Some("/backintime [me] - Reset trained skills and queue back to racial starter profile"), "Reset trained skills and queue back to racial starter profile", SlashHandler::Sync(crate::services::gm_soak_commands::dispatch_backintime_slash)),
    cmd!("ban", &[], true, ROLE_GM, "/ban <account_id> [duration_seconds] [reason]", Some("/ban <account_id> [duration_seconds] [reason] - Ban account"), "Ban account", SlashHandler::Sync(crate::services::auth_cache::dispatch_ban_slash)),
    cmd_icase!("campaign", &[], true, ROLE_GM, "/campaign <subcommand>", None, "Manage seasonal / live campaigns", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_campaign_slash)),
    cmd_icase!("chatban", &[], true, ROLE_GM, "/chatban <characterID> [minutes] [reason]", None, "Mute character from chat channels", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_chatban_slash)),
    cmd_icase!("chatunban", &[], true, ROLE_GM, "/chatunban <characterID>", None, "Remove chat ban from character", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_chatunban_slash)),
    cmd_icase!("cleartimers", &[], true, ROLE_GM, "/cleartimers", Some("/cleartimers - Clear active admin timers"), "Clear active admin timers", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_cleartimers_slash)),
    cmd_icase!("completeindustry", &[], true, ROLE_GM, "/completeindustry [charID]", Some("/finishjobs [charID] - Finish active industry jobs"), "Finish active industry jobs", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeindustry_slash)),
    cmd_icase!("completeskills", &[], true, ROLE_GM, "/completeskills [charID]", Some("/completeskills [charID] - Finish active skill training queue"), "Finish active skill training queue", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeskills_slash)),
    cmd!("contact", &[], true, ROLE_GM, "/contact <entityID> [standing]", None, "Set contact standing towards character, corp, or alliance", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_contact_slash)),
    cmd!("createccpevent", &[], true, ROLE_HIGH_GM, "/createccpevent <dateTime> <duration> <importance> <title> [| <description>]", None, "Broadcast a CCP calendar event notice", SlashHandler::Sync(crate::services::auth_cache::dispatch_createccpevent_slash)),
    cmd!("createitem", &[], true, ROLE_GM, "/createitem [me|<characterID>] <typeID> [quantity]", Some("/giveitem [me|<characterID>] <typeID> [quantity] | /createitem [me|<characterID>] <typeID> [quantity] - Spawn item into docked hangar or active ship cargo"), "Spawn item into docked hangar or active ship cargo", SlashHandler::Sync(crate::services::auth_cache::dispatch_giveitem_slash)),
    cmd!("deadwarp", &[], true, ROLE_GM, "/deadwarp [status|stop]", Some("/deadwarp [status|stop] - Warp ship to deterministic safe anchor ~2 AU off-grid and return"), "Warp ship to deterministic safe anchor ~2 AU off-grid and return", SlashHandler::Async(crate::services::gm_soak_commands::dispatch_deadwarp_slash)),
    cmd!("despawn", &[], true, ROLE_SPAWN_GM, "/despawn <entityID>", Some("/unspawn <entityID> - Remove spawned space entity"), "Remove spawned space entity", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_unspawn_slash)),
    cmd!("downtime", &[], true, ROLE_GM, "/downtime [seconds]", None, "Inspect or adjust server downtime countdown", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_downtime_slash)),
    cmd!("energy", &[], true, ROLE_GM, "/energy [charID|shipID] <amount>", None, "Set ship capacitor energy", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_energy_slash)),
    cmd_icase!("fastcheckout", &[], true, ROLE_GM, "/fastcheckout [itemID]", None, "Fast checkout market/NES fulfillment", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_fastcheckout_slash)),
    cmd_icase!("fastforward", &[], true, ROLE_GM, "/fastforward <seconds>", Some("/advancetime <seconds> - Advance simulation clock"), "Advance simulation clock", SlashHandler::Async(crate::services::notify_emitters_character_services::dispatch_advancetime_slash)),
    cmd_icase!("ff", &[], true, ROLE_GM, "/ff <seconds>", Some("/advancetime <seconds> - Advance simulation clock"), "Advance simulation clock", SlashHandler::Async(crate::services::notify_emitters_character_services::dispatch_advancetime_slash)),
    cmd_icase!("finishjobs", &[], true, ROLE_GM, "/finishjobs [charID]", Some("/finishjobs [charID] - Finish active industry jobs"), "Finish active industry jobs", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeindustry_slash)),
    cmd_icase!("finishskills", &[], true, ROLE_GM, "/finishskills [charID]", Some("/completeskills [charID] - Finish active skill training queue"), "Finish active skill training queue", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeskills_slash)),
    cmd_icase!("finishstructure", &[], true, ROLE_GM, "/finishstructure [structureID]", Some("/finishstructure [structureID] - Finish structure timer"), "Finish structure timer", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_upwelltimer_slash)),
    cmd!("fit", &[], true, ROLE_GM, "/fit <typeID> [slot]", Some("/fit <typeID> [slot] - Fit a module or rig onto active ship"), "Fit a module or rig onto active ship", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_fit_slash)),
    cmd_icase!("fwcapture", &[], true, ROLE_GM, "/fwcapture <solarSystemID> [factionID]", None, "Advance Factional Warfare system capture state", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_fwcapture_slash)),
    cmd!("getattr", &[], true, ROLE_QA_GM, "/getattr [me|<itemID>] <attribute>", Some("/getattr [me|<itemID>] <attribute> - Read dogma attribute on ship or item"), "Read dogma attribute on ship or item", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_getattr_slash)),
    cmd!("giveisk", &[], true, ROLE_GM, "/giveisk [me|<characterID>] <amount>", Some("/giveisk [me|<characterID>] <amount> - Add positive integer ISK to wallet"), "Add positive integer ISK to wallet", SlashHandler::Sync(crate::services::auth_cache::dispatch_giveisk_slash)),
    cmd!("giveitem", &[], true, ROLE_GM, "/giveitem [me|<characterID>] <typeID> [quantity]", Some("/giveitem [me|<characterID>] <typeID> [quantity] | /createitem [me|<characterID>] <typeID> [quantity] - Spawn item into docked hangar or active ship cargo"), "Spawn item into docked hangar or active ship cargo", SlashHandler::Sync(crate::services::auth_cache::dispatch_giveitem_slash)),
    cmd!("giveskill", &[], true, ROLE_GM, "/giveskill [me|<characterID>] <typeID> <0-5>", Some("/setskill [me|<characterID>] <typeID> <0-5> - Set character skill level"), "Set character skill level", SlashHandler::Sync(crate::services::auth_cache::dispatch_setskill_slash)),
    cmd!("heal", &[], true, ROLE_GM, "/heal [targetID]", Some("/heal [targetID] - Restore shield, armor, hull, and capacitor"), "Restore shield, armor, hull, and capacitor", SlashHandler::Async(crate::services::auth_cache::dispatch_heal_slash)),
    cmd!("help", &[], true, ROLE_GM, "/help [command]", Some("/help [command] - List available slash commands or show usage for a command"), "List available slash commands or show usage for a command", SlashHandler::Sync(crate::services::auth_cache::dispatch_help_slash)),
    cmd_icase!("industrytimer", &[], true, ROLE_GM, "/industrytimer [charID]", Some("/finishjobs [charID] - Finish active industry jobs"), "Finish active industry jobs", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeindustry_slash)),
    cmd!("kick", &[], true, ROLE_GM, "/kick <account_id> [reason]", Some("/kick <account_id> [reason] - Disconnect active account sessions"), "Disconnect active account sessions", SlashHandler::Sync(crate::services::auth_cache::dispatch_kick_slash)),
    cmd!("kill", &[], true, ROLE_GM, "/kill <entityID>", Some("/kill <entityID> - Destroy ship or space entity"), "Destroy ship or space entity", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_kill_slash)),
    cmd!("load", &[], true, ROLE_GM, "/load [me|<targetID>] <typeID> [quantity]", Some("/load [me|<targetID>] <typeID> [quantity] - Spawn item into active ship cargo or hangar"), "Spawn item into active ship cargo or hangar", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_load_slash)),
    cmd!("location", &[], true, ROLE_QA_GM, "/location [characterID]", Some("/location [characterID] - Show character location and coordinates"), "Show character location and coordinates", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_location_slash)),
    cmd!("moveme", &[], true, ROLE_QA_GM, "/moveme [systemID|stationID]", Some("/tr [me|<characterID>] <solarSystemID|stationID> [noblock] - Move to system or station"), "Move to system or station", SlashHandler::Async(crate::services::auth_cache::dispatch_teleport_slash)),
    cmd!("online", &[], true, ROLE_QA_GM, "/online [moduleID|all]", Some("/online [moduleID|all] - Online fitted modules"), "Online fitted modules", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_online_slash)),
    cmd!("operation", &[], true, ROLE_GM, "/operation <deactivate|replay|skip|reset_progress|reload_fsd|reveal_ui> ...", None, "Manage AIR NPE / Operations state", SlashHandler::Sync(crate::services::notify_emitters_operations_nodegraph::dispatch_operation_slash)),
    cmd!("pd", &[], true, ROLE_GM, "/pd <config|consensus|bonus> ...", None, "Manage Project Discovery progression", SlashHandler::Sync(crate::services::project_discovery::dispatch_pd_slash)),
    cmd!("reinforce", &[], true, ROLE_GM, "/reinforce <nodeID>", None, "Request solar system node reinforcement", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_reinforce_slash)),
    cmd!("role", &[], true, ROLE_HIGH_GM, "/role <account_id> <role_mask|name>", Some("/role <account_id> <role_mask|name> - Set account service role mask"), "Set account service role mask", SlashHandler::SyncMut(crate::services::auth_cache::dispatch_role_slash)),
    cmd!("season", &[], true, ROLE_GM, "/season <addpoints|completechallenge|reset> ...", None, "Manage Seasonal Progression / Agency points", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_season_slash)),
    cmd!("secstatus", &[], true, ROLE_GM, "/secstatus [charID] <value>", None, "Set character security status", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_secstatus_slash)),
    cmd!("session", &[], true, ROLE_GM, "/session | /session ledger <character|me> [limit]", Some("/session | /session ledger <character|me> [limit] - Inspect session dict, wire state, and outbound ledger"), "Inspect session dict, wire state, and outbound ledger", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_session_slash)),
    cmd!("setattr", &[], true, ROLE_GM, "/setattr [me|<itemID>] <attribute> <value>", Some("/setattr [me|<itemID>] <attribute> <value> - Set dogma attribute"), "Set dogma attribute", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_setattr_slash)),
    cmd!("setomega", &[], true, ROLE_GM, "/setomega <userID> <until_unix>", Some("/setomega <userID> <until_unix> - Set Omega clone expiration timestamp"), "Set Omega clone expiration timestamp", SlashHandler::Sync(crate::services::account_services::dispatch_account_slash)),
    cmd!("setskill", &[], true, ROLE_GM, "/setskill [me|<characterID>] <typeID> <0-5>", Some("/setskill [me|<characterID>] <typeID> <0-5> - Set character skill level"), "Set character skill level", SlashHandler::Sync(crate::services::auth_cache::dispatch_setskill_slash)),
    cmd!("setunderage", &[], true, ROLE_GM, "/setunderage <userID> <1|true|0|false>", Some("/setunderage <userID> <1|true|0|false> - Set account underage flag"), "Set account underage flag", SlashHandler::Sync(crate::services::account_services::dispatch_account_slash)),
    cmd_icase!("shutdown", &[], true, ROLE_GM, "/shutdown [seconds|cancel]", None, "Schedule or cancel cluster shutdown", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_shutdown_slash)),
    cmd!("soak", &[], true, ROLE_GM, "/soak logi <count|status|stop>", Some("/soak logi <count|status|stop> - Spawn or manage NPC logistics soak ring around GM ship"), "Spawn or manage NPC logistics soak ring around GM ship", SlashHandler::Async(crate::services::gm_soak_commands::dispatch_soak_slash)),
    cmd!("spawn", &[], true, ROLE_SPAWN_GM, "/spawn [me|<characterID>] <typeID> [qty]", Some("/spawn [me|<characterID>] <typeID> [qty] | /spawnnpc [me|<characterID>] <typeID> - Spawn NPC/entity in system"), "Spawn entity or NPC in system", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_spawn_slash)),
    cmd_icase!("spawnanomaly", &[], true, ROLE_GM, "/spawnanomaly [type]", None, "Spawn cosmic anomaly in current solar system", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_spawnanomaly_slash)),
    cmd!("spawnnpc", &[], true, ROLE_SPAWN_GM, "/spawnnpc [me|<characterID>] <typeID>", Some("/spawn [me|<characterID>] <typeID> [qty] | /spawnnpc [me|<characterID>] <typeID> - Spawn NPC/entity in system"), "Spawn NPC entity in system", SlashHandler::Async(crate::services::auth_cache::dispatch_spawnnpc_slash)),
    cmd!("spawnwreck", &[], true, ROLE_GM, "/spawnwreck <typeID> [count]", Some("/spawnwreck <typeID> [count] - Spawn lootable wrecks with deterministic salvage and hull materials"), "Spawn lootable wrecks with deterministic salvage and hull materials", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_spawnwreck_slash)),
    cmd!("standing", &[], true, ROLE_GM, "/standing <entityID> [value]", None, "Set contact standing towards character, corp, or alliance", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_contact_slash)),
    cmd_icase!("structure", &[], true, ROLE_STRUCTURE_GM, "/structure <subcommand> <structureID> ...", None, "Manage Upwell structure state and timers", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_structure_slash)),
    cmd_icase!("structuretimer", &[], true, ROLE_GM, "/structuretimer [structureID]", Some("/finishstructure [structureID] - Finish structure timer"), "Finish structure timer", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_upwelltimer_slash)),
    cmd!("survey", &[], true, ROLE_QA_GM, "/survey <schedule|activate|complete> ...", None, "Manage player survey status", SlashHandler::Sync(crate::services::operations_survey_handlers::dispatch_survey_slash)),
    cmd!("suspend", &[], true, ROLE_GM, "/suspend <account_id> <duration_seconds> [reason]", Some("/suspend <account_id> <duration_seconds> [reason] - Temporarily suspend account"), "Temporarily suspend account", SlashHandler::Sync(crate::services::auth_cache::dispatch_suspend_slash)),
    cmd!("sysjunkclear", &[], true, ROLE_GM, "/sysjunkclear", Some("/sysjunkclear - Purge wrecks, jetcans, and expired deployables in current solar system"), "Purge wrecks, jetcans, and expired deployables in current solar system", SlashHandler::Async(crate::services::gm_soak_commands::dispatch_sysjunkclear_slash)),
    cmd!("teleport", &[], true, ROLE_GM, "/teleport [me|<characterID>] <solarSystemID|stationID> [noblock]", Some("/tr [me|<characterID>] <solarSystemID|stationID> [noblock] - Move to system or station"), "Move to system or station", SlashHandler::Async(crate::services::auth_cache::dispatch_teleport_slash)),
    cmd!("tidi", &[], true, ROLE_GM, "/tidi [factor]", None, "Inspect or set solar system Time Dilation factor", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_tidi_slash)),
    cmd!("tr", &[], true, ROLE_GM, "/tr [me|<characterID>] <solarSystemID|stationID> [noblock]", Some("/tr [me|<characterID>] <solarSystemID|stationID> [noblock] - Move to system or station"), "Move to system or station", SlashHandler::Async(crate::services::auth_cache::dispatch_teleport_slash)),
    cmd_icase!("trainskills", &[], true, ROLE_GM, "/trainskills [charID]", Some("/completeskills [charID] - Finish active skill training queue"), "Finish active skill training queue", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_completeskills_slash)),
    cmd!("translocate", &[], true, ROLE_QA_GM, "/translocate [me|<characterID>] <solarSystemID|stationID> [noblock]", Some("/tr [me|<characterID>] <solarSystemID|stationID> [noblock] - Move to system or station"), "Move to system or station", SlashHandler::Async(crate::services::auth_cache::dispatch_teleport_slash)),
    cmd!("unban", &[], true, ROLE_GM, "/unban <account_id>", Some("/unban <account_id> - Remove account ban"), "Remove account ban", SlashHandler::Sync(crate::services::auth_cache::dispatch_unban_slash)),
    cmd!("unspawn", &[], true, ROLE_SPAWN_GM, "/unspawn <entityID>", Some("/unspawn <entityID> - Remove spawned space entity"), "Remove spawned space entity", SlashHandler::Async(crate::services::character_skills::character_gm_tools::dispatch_unspawn_slash)),
    cmd!("unsuspend", &[], true, ROLE_GM, "/unsuspend <account_id>", Some("/unsuspend <account_id> - Remove account suspension"), "Remove account suspension", SlashHandler::Sync(crate::services::auth_cache::dispatch_unsuspend_slash)),
    cmd_icase!("upwelltimer", &[], true, ROLE_GM, "/upwelltimer [structureID]", Some("/finishstructure [structureID] - Finish structure timer"), "Finish structure timer", SlashHandler::Sync(crate::services::notify_emitters_character_services::dispatch_upwelltimer_slash)),
    cmd!("whoami", &[], true, ROLE_QA_GM, "/whoami", Some("/whoami - Show current session character, corp, location, ship, and role"), "Show current session character, corp, location, ship, and role", SlashHandler::Sync(crate::services::character_skills::character_gm_tools::dispatch_whoami_slash)),
];

const fn count_advertised(entries: &[SlashCommandEntry]) -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < entries.len() {
        if entries[i].advertised {
            count += 1;
        }
        i += 1;
    }
    count
}

const ADVERTISED_COMMAND_COUNT: usize = count_advertised(SLASH_COMMAND_REGISTRY);

const fn build_remote_slash_commands() -> [&'static str; ADVERTISED_COMMAND_COUNT] {
    let mut out = [""; ADVERTISED_COMMAND_COUNT];
    let mut i = 0;
    let mut k = 0;
    while i < SLASH_COMMAND_REGISTRY.len() {
        if SLASH_COMMAND_REGISTRY[i].advertised {
            out[k] = SLASH_COMMAND_REGISTRY[i].name;
            k += 1;
        }
        i += 1;
    }
    out
}

const DERIVED_REMOTE_SLASH_COMMANDS: [&'static str; ADVERTISED_COMMAND_COUNT] =
    build_remote_slash_commands();

/// All advertised slash command names derived directly from [`SLASH_COMMAND_REGISTRY`].
pub const fn all_remote_slash_commands() -> &'static [&'static str] {
    &DERIVED_REMOTE_SLASH_COMMANDS
}

/// Parses `cmd` into a [`SlashInvocation`] when `cmd.trim()` begins with `'/'`.
///
/// This is the canonical leading-slash parser for the server.
pub fn parse_slash_invocation(cmd: &str) -> Option<SlashInvocation<'_>> {
    let trimmed = cmd.trim();
    let without_slash = trimmed.strip_prefix('/')?;
    let split_idx = without_slash
        .find(char::is_whitespace)
        .unwrap_or(without_slash.len());
    let (head_raw, after_head) = without_slash.split_at(split_idx);
    let had_whitespace_after_head = after_head.starts_with(char::is_whitespace);
    Some(SlashInvocation {
        raw: trimmed,
        head_raw,
        head_lower: head_raw.to_ascii_lowercase(),
        rest: after_head.trim(),
        had_whitespace_after_head,
    })
}

/// Returns `true` when `cmd.trim_start()` begins with `'/'`.
#[inline]
pub fn is_slash_command(cmd: &str) -> bool {
    cmd.trim_start().starts_with('/')
}

/// Returns `true` when `cmd.trim()` is bare `"/"` (remote command list query).
#[inline]
pub fn is_slash_list_query(cmd: &str) -> bool {
    cmd.trim() == "/"
}

/// Looks up a registry entry by exact command head (preserving case-sensitivity parity with
/// `required_slash_role`). Accepts either `"/whoami"` or `"whoami"`.
pub fn lookup_slash_entry(cmd: &str) -> Option<&'static SlashCommandEntry> {
    let head = if let Some(inv) = parse_slash_invocation(cmd) {
        inv.head_raw
    } else {
        let trimmed = cmd.trim();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        &trimmed[..end]
    };
    if head.is_empty() {
        return None;
    }
    SLASH_COMMAND_REGISTRY
        .iter()
        .find(|entry| entry.name == head || entry.aliases.contains(&head))
}

/// Looks up a registry entry case-insensitively on command head (accepting with or without `'/'`).
pub fn lookup_slash_entry_icase(cmd: &str) -> Option<&'static SlashCommandEntry> {
    let head = if let Some(inv) = parse_slash_invocation(cmd) {
        inv.head_raw
    } else {
        let trimmed = cmd.trim();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        &trimmed[..end]
    };
    if head.is_empty() {
        return None;
    }
    SLASH_COMMAND_REGISTRY.iter().find(|entry| {
        entry.name.eq_ignore_ascii_case(head)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(head))
    })
}

/// Returns the required role bitmask for `cmd`, derived directly from [`SLASH_COMMAND_REGISTRY`].
///
/// Matches legacy `auth_cache::required_slash_role` semantics:
/// - Bare `"/"` requires `ROLE_ANY_REMOTE`.
/// - Empty or unrecognized casing/command defaults to `service_roles::ROLE_GM_MASK`.
pub fn required_slash_role(cmd: &str) -> u64 {
    let trimmed = cmd.trim();
    if trimmed == "/" {
        return ROLE_ANY_REMOTE;
    }
    let Some(inv) = parse_slash_invocation(trimmed) else {
        return service_roles::ROLE_GM_MASK;
    };
    if inv.head_raw.is_empty() {
        return ROLE_ANY_REMOTE;
    }
    if let Some(entry) = lookup_slash_entry(trimmed) {
        return entry.min_role;
    }
    // Preserve exact-case semantics for commands broader than `ROLE_GM_MASK` (e.g.
    // `/WHOAMI` -> `ROLE_GM_MASK`), while preventing case variations such as `/STRUCTURE`
    // from weakening commands restricted to a narrower GM role subset.
    if let Some(icase_entry) = lookup_slash_entry_icase(trimmed) {
        if (icase_entry.min_role & service_roles::ROLE_GM_MASK) != service_roles::ROLE_GM_MASK {
            return icase_entry.min_role;
        }
    }
    service_roles::ROLE_GM_MASK
}

/// Returns all advertised slash command names permitted for `role`, derived from [`SLASH_COMMAND_REGISTRY`].
pub fn remote_slash_commands_for_role(role: u64) -> Vec<&'static str> {
    all_remote_slash_commands()
        .iter()
        .copied()
        .filter(|name| {
            let with_slash = format!("/{name}");
            (role & required_slash_role(&with_slash)) != 0
        })
        .collect()
}

/// Formats the `SlashError` remote command list message (`"Commands: [...]"`).
pub fn format_remote_command_list_reason(role: u64) -> String {
    let quoted: Vec<String> = remote_slash_commands_for_role(role)
        .into_iter()
        .map(|cmd| format!("'{cmd}'"))
        .collect();
    format!("Commands: [{}]", quoted.join(", "))
}

/// Returns the detailed usage string for command `name` (with or without leading `'/'`),
/// preserving legacy `slash_command_usage` semantics.
pub fn slash_command_usage(name: &str) -> Option<&'static str> {
    lookup_slash_entry_icase(name).and_then(|e| e.detailed_usage)
}

/// Formats `/help` output for `role`, optionally scoped to a specific `topic`,
/// preserving full parity with legacy `/help` and `/help <cmd>` responses.
pub fn format_slash_help(role: u64, topic: Option<&str>) -> String {
    let cmds = remote_slash_commands_for_role(role);
    let normalized = topic
        .map(|raw| raw.trim().trim_start_matches('/').to_ascii_lowercase())
        .filter(|name| !name.is_empty());
    if let Some(name) = normalized {
        if !cmds.contains(&name.as_str()) {
            if all_remote_slash_commands().contains(&name.as_str()) {
                return format!(
                    "/{name}: requires a higher role than your current session role. Use /help to list available commands."
                );
            }
            return format!(
                "Unknown slash command '/{name}'. Use /help to list available commands."
            );
        }
        if let Some(usage) = slash_command_usage(&name) {
            return usage.to_string();
        }
        return format!(
            "/{name}: available to your role (no detailed usage entry). Use /help to list commands."
        );
    }
    let joined = cmds
        .iter()
        .map(|c| format!("/{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Available slash commands ({}): {joined}. Use /help <command> for usage.",
        cmds.len()
    )
}

fn slash_list_error(role: u64) -> BridgeRpcError {
    let reason = format_remote_command_list_reason(role);
    BridgeRpcError::user_error_with_args(UserErrorKey::SlashError, [("reason", UserErrorArg::Str(reason))])
}

fn insufficient_slash_role_error(
    service: &str,
    cmd: &str,
    req_role: u64,
    actual_role: u64,
) -> BridgeRpcError {
    BridgeRpcError::insufficient_role(
        req_role,
        actual_role,
        format!("{service}.SlashCmd"),
        format!("{service}.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
    )
}

pub(crate) fn unknown_slash_error(cmd: &str) -> BridgeRpcError {
    let msg = format!("Unknown or unsupported slash command '{cmd}'");
    let reason = format!("UnknownSlashCommand: {msg}");
    BridgeRpcError::with_client_error(
        "UnknownSlashCommand",
        msg,
        ClientError::user_with_args(UserErrorKey::SlashError, [("reason", UserErrorArg::Str(reason))]),
    )
}

fn invalid_slash_arg_error(msg: impl Into<String>) -> BridgeRpcError {
    let msg = msg.into();
    let reason = format!("InvalidSlashArgument: {msg}");
    BridgeRpcError::with_client_error(
        "InvalidSlashArgument",
        msg,
        ClientError::user_with_args(UserErrorKey::SlashError, [("reason", UserErrorArg::Str(reason))]),
    )
}

const GIVEISK_USAGE: &str = "Usage: /giveisk [me|<character_id>] <positive_integer_amount>";
const GIVEITEM_USAGE: &str =
    "Usage: /giveitem [me|<characterID>] <type_id> [quantity] or /createitem [me|<characterID>] <type_id> [quantity]";
const SETSKILL_USAGE: &str =
    "Usage: /setskill [me|<characterID>] <typeID> <0-5> or /giveskill [me|<characterID>] <typeID> <0-5>";

fn parse_optional_target_char(caller_cid: i64, target: &str) -> Option<i64> {
    if target.eq_ignore_ascii_case("me") || target.eq_ignore_ascii_case("self") {
        Some(caller_cid)
    } else {
        target.parse::<i64>().ok().filter(|&id| id > 0)
    }
}

/// Typed arguments parsed for `/giveisk [me|self|<character_id>] <positive_integer_amount>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GiveIskArgs {
    pub target_char_id: i64,
    pub amount_cents: eve_db::IskCents,
}

impl GiveIskArgs {
    /// Parses `/giveisk` arguments without mutating world state, rejecting bad targets,
    /// non-integer or non-positive amounts, and cent-overflow values.
    pub(crate) fn parse(caller_cid: i64, rest: &str) -> Result<Self, BridgeRpcError> {
        let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
        let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let (target_char_id, amount_tok) = match token_strs.as_slice() {
            [amt] => (caller_cid, *amt),
            [target, amt] => {
                let Some(target_id) = parse_optional_target_char(caller_cid, target) else {
                    return Err(invalid_slash_arg_error(GIVEISK_USAGE));
                };
                (target_id, *amt)
            }
            _ => return Err(invalid_slash_arg_error(GIVEISK_USAGE)),
        };
        let Ok(amount_isk) = amount_tok.parse::<i64>() else {
            return Err(invalid_slash_arg_error(GIVEISK_USAGE));
        };
        if amount_isk <= 0 {
            return Err(invalid_slash_arg_error("Amount for /giveisk must be positive"));
        }
        let Some(raw_cents) = amount_isk.checked_mul(100) else {
            return Err(BridgeRpcError::with_client_error(
                "BalanceOverflow",
                "Adding ISK would overflow balance",
                ClientError::user_with_args(
                    UserErrorKey::SlashError,
                    [(
                        "reason",
                        UserErrorArg::Str("BalanceOverflow: Adding ISK would overflow balance".to_string()),
                    )],
                ),
            ));
        };
        Ok(Self {
            target_char_id,
            amount_cents: eve_db::IskCents::from_raw_cents(raw_cents),
        })
    }
}

/// Typed arguments parsed for `/setskill` and `/giveskill` (`[me|self|<characterID>] <typeID> <0-5>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SetSkillArgs {
    pub target_char_id: i64,
    pub skill_type_id: eve_sde::TypeId,
    pub level: u8,
}

impl SetSkillArgs {
    /// Parses `/setskill` or `/giveskill` arguments without mutating world state, validating
    /// target character syntax, positive `typeID`, and bounded skill level `0..=5`.
    pub(crate) fn parse(caller_cid: i64, rest: &str) -> Result<Self, BridgeRpcError> {
        let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
        let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let (target_char_id, skill_id_str, lvl_str) = match token_strs.as_slice() {
            [s, l] => (caller_cid, *s, *l),
            [target, s, l] => {
                let Some(target_id) = parse_optional_target_char(caller_cid, target) else {
                    return Err(invalid_slash_arg_error(SETSKILL_USAGE));
                };
                (target_id, *s, *l)
            }
            _ => return Err(invalid_slash_arg_error(SETSKILL_USAGE)),
        };
        let Some(skill_type_id) = skill_id_str.parse::<u32>().ok().filter(|&id| id > 0) else {
            return Err(invalid_slash_arg_error("Invalid skill_id for /setskill"));
        };
        let Ok(level) = lvl_str.parse::<u8>() else {
            return Err(invalid_slash_arg_error("Invalid level for /setskill"));
        };
        if level > 5 {
            return Err(invalid_slash_arg_error("Level for /setskill must be between 0 and 5"));
        }
        Ok(Self {
            target_char_id,
            skill_type_id,
            level,
        })
    }
}

/// Syntax-level target/argument representation for `/giveitem` and `/createitem`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GiveItemArgs {
    Explicit {
        target_char_id: i64,
        type_id: u32,
        quantity: i32,
    },
    /// Two positive integers `[a, b]` could mean either `<charID> <typeID>` (qty = 1) or
    /// `<typeID> <quantity>` targeting the caller; resolved against world characters + SDE.
    AmbiguousNumericPair {
        caller_char_id: i64,
        first: i64,
        second: i32,
    },
}

/// Fully resolved arguments for `/giveitem` and `/createitem`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedGiveItemArgs {
    pub target_char_id: i64,
    pub type_id: u32,
    pub quantity: i32,
}

impl GiveItemArgs {
    /// Parses `/giveitem` or `/createitem` syntax without mutating world state, validating
    /// target tokens, positive `typeID`, and positive `i32` quantity.
    pub(crate) fn parse(caller_cid: i64, rest: &str) -> Result<Self, BridgeRpcError> {
        let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
        let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        match token_strs.as_slice() {
            [type_str] => {
                let Some(type_id) = type_str.parse::<u32>().ok().filter(|&id| id > 0) else {
                    return Err(invalid_slash_arg_error("Invalid type_id for /giveitem"));
                };
                Ok(Self::Explicit {
                    target_char_id: caller_cid,
                    type_id,
                    quantity: 1,
                })
            }
            [who, type_str, qty_str] => {
                let Some(target_char_id) = parse_optional_target_char(caller_cid, who) else {
                    return Err(invalid_slash_arg_error(GIVEITEM_USAGE));
                };
                let Some(type_id) = type_str.parse::<u32>().ok().filter(|&id| id > 0) else {
                    return Err(invalid_slash_arg_error("Invalid type_id for /giveitem"));
                };
                let Ok(quantity) = qty_str.parse::<i32>() else {
                    return Err(invalid_slash_arg_error("Quantity for /giveitem must be positive"));
                };
                if quantity <= 0 {
                    return Err(invalid_slash_arg_error("Quantity for /giveitem must be positive"));
                }
                Ok(Self::Explicit {
                    target_char_id,
                    type_id,
                    quantity,
                })
            }
            [tok0, tok1] => {
                if tok0.eq_ignore_ascii_case("me") || tok0.eq_ignore_ascii_case("self") {
                    let Some(type_id) = tok1.parse::<u32>().ok().filter(|&id| id > 0) else {
                        return Err(invalid_slash_arg_error("Invalid type_id for /giveitem"));
                    };
                    return Ok(Self::Explicit {
                        target_char_id: caller_cid,
                        type_id,
                        quantity: 1,
                    });
                }
                let Some(first) = tok0.parse::<i64>().ok().filter(|&id| id > 0) else {
                    return Err(invalid_slash_arg_error(GIVEITEM_USAGE));
                };
                let Ok(second) = tok1.parse::<i32>() else {
                    return Err(invalid_slash_arg_error("Quantity for /giveitem must be positive"));
                };
                if second <= 0 {
                    return Err(invalid_slash_arg_error("Quantity for /giveitem must be positive"));
                }
                Ok(Self::AmbiguousNumericPair {
                    caller_char_id: caller_cid,
                    first,
                    second,
                })
            }
            _ => Err(invalid_slash_arg_error(GIVEITEM_USAGE)),
        }
    }

    /// Resolves ambiguous two-integer invocations against known characters and SDE types.
    pub(crate) fn resolve(
        self,
        sde: &eve_sde::SdeRegistry,
        char_exists: impl FnOnce(i64) -> bool,
    ) -> Result<ResolvedGiveItemArgs, crate::domain::DomainError> {
        match self {
            Self::Explicit {
                target_char_id,
                type_id,
                quantity,
            } => Ok(ResolvedGiveItemArgs {
                target_char_id,
                type_id,
                quantity,
            }),
            Self::AmbiguousNumericPair {
                caller_char_id,
                first,
                second,
            } => {
                if char_exists(first) {
                    return Ok(ResolvedGiveItemArgs {
                        target_char_id: first,
                        type_id: second as u32,
                        quantity: 1,
                    });
                }
                let second_is_valid_type = sde
                    .types
                    .get(&(second as u32))
                    .is_some_and(|rec| rec.category_id != 0);
                let first_is_known_type = u32::try_from(first)
                    .ok()
                    .is_some_and(|t| sde.types.contains_key(&t));
                if !first_is_known_type && second_is_valid_type {
                    return Err(crate::domain::DomainError::not_found(
                        "CharacterNotFound",
                        format!("Character {first} not found"),
                    ));
                }
                if let Ok(type_id) = u32::try_from(first) {
                    Ok(ResolvedGiveItemArgs {
                        target_char_id: caller_char_id,
                        type_id,
                        quantity: second,
                    })
                } else {
                    Err(crate::domain::DomainError::not_found(
                        "CharacterNotFound",
                        format!("Character {first} not found"),
                    ))
                }
            }
        }
    }
}

const HEAL_USAGE: &str = "Usage: /heal [me|self|myship|<targetID>]";
const KILL_USAGE: &str = "Usage: /kill <me|self|myship|targetID>";

fn is_self_ship_alias(tok: &str) -> bool {
    crate::services::character_skills::character_gm_tools::is_me_or_myship(tok)
        || tok.eq_ignore_ascii_case("self")
}

/// Target parsed for `/heal [me|self|myship|<targetID>]` (`#1193`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HealSlashTarget {
    SelfShip,
    EntityOrCharId(i64),
}

/// Typed arguments parsed for `/heal` (`#1193`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HealSlashArgs {
    pub target: HealSlashTarget,
}

/// Parses `/heal` arguments, accepting empty args, self aliases, or a single positive entity/character ID.
pub(crate) fn parse_heal_slash_args(rest: &str) -> Result<HealSlashArgs, BridgeRpcError> {
    let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
    let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    match token_strs.as_slice() {
        [] => Ok(HealSlashArgs {
            target: HealSlashTarget::SelfShip,
        }),
        [tok] if is_self_ship_alias(tok) => Ok(HealSlashArgs {
            target: HealSlashTarget::SelfShip,
        }),
        [tok] => tok
            .parse::<i64>()
            .ok()
            .filter(|&id| id > 0)
            .map(|id| HealSlashArgs {
                target: HealSlashTarget::EntityOrCharId(id),
            })
            .ok_or_else(|| invalid_slash_arg_error(HEAL_USAGE)),
        _ => Err(invalid_slash_arg_error(HEAL_USAGE)),
    }
}

/// Target parsed for `/kill <me|self|myship|targetID>` (`#1193`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KillSlashTarget {
    SelfChar,
    EntityOrCharId(i64),
}

/// Typed arguments parsed for `/kill` (`#1193`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KillSlashArgs {
    pub target: KillSlashTarget,
}

/// Parses `/kill` arguments, requiring either a self alias or a single positive target entity/character ID.
pub(crate) fn parse_kill_slash_args(rest: &str) -> Result<KillSlashArgs, BridgeRpcError> {
    let tokens = crate::services::character_skills::character_gm_tools::parse_slash_tokens(rest);
    let token_strs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    match token_strs.as_slice() {
        [tok] if is_self_ship_alias(tok) => Ok(KillSlashArgs {
            target: KillSlashTarget::SelfChar,
        }),
        [tok] => tok
            .parse::<i64>()
            .ok()
            .filter(|&id| id > 0)
            .map(|id| KillSlashArgs {
                target: KillSlashTarget::EntityOrCharId(id),
            })
            .ok_or_else(|| invalid_slash_arg_error(KILL_USAGE)),
        _ => Err(invalid_slash_arg_error(KILL_USAGE)),
    }
}

enum PreparedSlashDispatch<'a> {
    Immediate(Result<PyRep, BridgeRpcError>),
    Execute {
        cid: i64,
        cmd: &'a str,
        inv: SlashInvocation<'a>,
        handler: SlashHandler,
    },
}

fn prepare_slash_dispatch<'a>(
    ctx: &crate::router::ServerContext,
    session: &eve_machonet::SessionState,
    service: &str,
    cmd: &'a str,
) -> PreparedSlashDispatch<'a> {
    if is_slash_list_query(cmd) {
        if service == "publicQaToolsServer" {
            let (_, cid) = match crate::router::ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return PreparedSlashDispatch::Immediate(Err(e)),
            };
            let req_role = required_slash_role(cmd);
            if !crate::router::ServerContext::has_gm_role_mask(session.role, req_role) {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
                return PreparedSlashDispatch::Immediate(Err(insufficient_slash_role_error(
                    service,
                    cmd,
                    req_role,
                    session.role,
                )));
            }
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
            return PreparedSlashDispatch::Immediate(Err(slash_list_error(session.role)));
        }
        if let Err(e) = crate::router::ServerContext::require_auth(session) {
            return PreparedSlashDispatch::Immediate(Err(e));
        }
        return PreparedSlashDispatch::Immediate(Err(slash_list_error(session.role)));
    }

    let (_, cid) = match crate::router::ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return PreparedSlashDispatch::Immediate(Err(e)),
    };

    let req_role = required_slash_role(cmd);
    if !crate::router::ServerContext::has_gm_role_mask(session.role, req_role) {
        crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
        return PreparedSlashDispatch::Immediate(Err(insufficient_slash_role_error(
            service,
            cmd,
            req_role,
            session.role,
        )));
    }

    let Some(inv) = parse_slash_invocation(cmd) else {
        crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
        return PreparedSlashDispatch::Immediate(Err(unknown_slash_error(cmd)));
    };

    if let Some(entry) = lookup_slash_entry(inv.head_raw) {
        return PreparedSlashDispatch::Execute {
            cid,
            cmd,
            inv,
            handler: entry.handler,
        };
    }

    // `notify_emitters_character_services` commands (e.g. `/STRUCTURE`, `/FastCheckout`)
    // match heads case-insensitively while `character_gm_tools` / `gm_soak_commands` rows
    // match exact-case (e.g. `/WHOAMI` fails).
    if let Some(icase_entry) = lookup_slash_entry_icase(inv.head_raw) {
        if icase_entry.case_insensitive {
            return PreparedSlashDispatch::Execute {
                cid,
                cmd,
                inv,
                handler: icase_entry.handler,
            };
        }
    }

    crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
    PreparedSlashDispatch::Immediate(Err(unknown_slash_error(cmd)))
}

/// Central single-entry synchronous dispatcher for `slash.SlashCmd` and `publicQaToolsServer.SlashCmd` (`#1193`).
///
/// Parses the command head once, enforces authentication/character selection and the row's
/// `min_role`, invokes bound [`SlashHandler::Sync`] entries directly, yields
/// [`SlashDispatchOutcome::RequiresAsync`] for `SyncMut`/`Async` entries requiring
/// [`dispatch_slash_command_async`], and rejects unknown slash commands.
pub fn dispatch_slash_command(
    ctx: &crate::router::ServerContext,
    session: &eve_machonet::SessionState,
    service: &str,
    args: &[PyRep],
) -> SlashDispatchOutcome {
    let cmd = crate::router::rpc_args::RpcArgs::positional(args)
        .get_raw(0, "cmd")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .trim();
    match prepare_slash_dispatch(ctx, session, service, cmd) {
        PreparedSlashDispatch::Immediate(res) => SlashDispatchOutcome::Handled(res),
        PreparedSlashDispatch::Execute {
            cid,
            cmd,
            inv,
            handler: SlashHandler::Sync(handler),
        } => {
            let res = handler(ctx, session, &inv);
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, res.is_ok());
            SlashDispatchOutcome::Handled(res)
        }
        PreparedSlashDispatch::Execute {
            handler: SlashHandler::SyncMut(_) | SlashHandler::Async(_),
            ..
        } => SlashDispatchOutcome::RequiresAsync,
    }
}

/// Central single-entry asynchronous dispatcher for `slash.SlashCmd` and
/// `publicQaToolsServer.SlashCmd` (`#1193`).
///
/// Executes synchronous (`Sync` / `SyncMut`) and asynchronous (`Async`) bound registry handlers
/// while sharing identical list-query, character-selection, role-gate, audit, and unknown-command
/// rejection behavior with [`dispatch_slash_command`].
pub async fn dispatch_slash_command_async(
    ctx: &crate::router::ServerContext,
    session: &mut eve_machonet::SessionState,
    service: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let cmd = crate::router::rpc_args::RpcArgs::positional(args)
        .get_raw(0, "cmd")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .trim();
    match prepare_slash_dispatch(ctx, session, service, cmd) {
        PreparedSlashDispatch::Immediate(res) => res,
        PreparedSlashDispatch::Execute {
            cid,
            cmd,
            inv,
            handler,
        } => {
            let res = match handler {
                SlashHandler::Sync(f) => f(ctx, session, &inv),
                SlashHandler::SyncMut(f) => f(ctx, session, &inv),
                SlashHandler::Async(f) => f(ctx, session, &inv).await,
            };
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, res.is_ok());
            res
        }
    }
}

#[cfg(test)]
pub(crate) fn parse_slash_invocation_preserving_trailing_ws(
    cmd: &str,
) -> Option<SlashInvocation<'_>> {
    let trimmed_start = cmd.trim_start();
    let without_slash = trimmed_start.strip_prefix('/')?;
    let split_idx = without_slash
        .find(char::is_whitespace)
        .unwrap_or(without_slash.len());
    let (head_raw, after_head) = without_slash.split_at(split_idx);
    let had_whitespace_after_head = after_head.starts_with(char::is_whitespace);
    Some(SlashInvocation {
        raw: trimmed_start.trim_end(),
        head_raw,
        head_lower: head_raw.to_ascii_lowercase(),
        rest: after_head.trim(),
        had_whitespace_after_head,
    })
}

#[cfg(test)]
pub(crate) async fn dispatch_registered_for_test(
    ctx: &crate::router::ServerContext,
    session: &mut eve_machonet::SessionState,
    inv: &SlashInvocation<'_>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let entry = lookup_slash_entry(inv.head_raw)
        .or_else(|| lookup_slash_entry_icase(inv.head_raw).filter(|e| e.case_insensitive))?;
    let res = match entry.handler {
        SlashHandler::Sync(f) => f(ctx, session, inv),
        SlashHandler::SyncMut(f) => f(ctx, session, inv),
        SlashHandler::Async(f) => f(ctx, session, inv).await,
    };
    Some(res)
}

#[cfg(test)]
#[path = "slash_registry_tests.rs"]
mod slash_registry_tests;
