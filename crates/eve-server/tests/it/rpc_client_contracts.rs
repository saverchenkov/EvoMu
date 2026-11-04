//! Reverse-engineered V24.01 client RPC & Notification contract acceptance suite.
//!
//! Loads `.agents/skills/evomu-adversarial-review/references/parity/sources/client_expectations.json`
//! (extracted directly from the V24.01 client bytecode `code.ccp`), seeds non-empty world state
//! across all server domains, dispatches each RPC call-site signature, and validates every returned
//! `PyRep` and emitted `On*` notification against the client's consumer expectations (`reply_shape`
//! and `notify.reads`).

use crate::common;
use eve_db::*;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const CHAR_ID: i64 = 90_000_001;
const OTHER_CHAR_ID: i64 = 90_000_002;
const CORP_ID: i64 = 98_000_001;
const OTHER_CORP_ID: i64 = 98_000_002;
const ALLIANCE_ID: i64 = 99_000_001;
const STATION_ID: i64 = 60_003_760;
const SYSTEM_ID: u32 = 30_000_142;
const SHIP_ID: i64 = 100_000_001;
const MODULE_ID: i64 = 100_000_005;
const HANGAR_ITEM_ID: i64 = 100_000_003;
const BPO_ITEM_ID: i64 = 100_000_007;
const STRUCTURE_ID: i64 = 1_030_000_000_001;
const PLANET_ID: i64 = 40_009_082;
const FLEET_ID: i64 = 500_001;
const WING_ID: i64 = 501_001;
const SQUAD_ID: i64 = 502_001;
const CONTRACT_ID: i64 = 600_001;
const WAR_ID: i64 = 1;
const AGENT_ID: i64 = 3_008_416;
const FOLDER_ID: i64 = 910_001;
const BOOKMARK_ID: i64 = 900_001;

/// Client-only methods on `invCacheContainer` or client-internal debug hooks that are not server RPCs.
const CLIENT_ONLY_PSEUDO_RPCS: &[&str] = &[
    "DB2.CallProc",
    "debug.Exec",
    "cache.Rowset",
    "cache.Setting",
    "invbroker.GetCapacity",
    "invbroker.GetItem",
    "invbroker.GetSelfInvItem",
    "invbroker.ListByFlags",
    "invbroker.ListCargo",
    "invbroker.ListDroneBay",
    "invbroker.ListFighterBay",
    "invbroker.ListHardwareModules",
    "invbroker.OnItemChange",
    "invbroker.typeID",
];

/// RPCs that are intentionally `UnimplementedRPC` (`removed_from_live`, `dead_client_code`, or
/// documented FSD data gaps in `supervisor_triage.json`) and are explicitly asserted by existing
/// negative tests to return `UnimplementedRPC`.
const INTENTIONALLY_UNIMPLEMENTED_RPCS: &[&str] = &[
    "RWManager.get_closest_rw_stations",
    "RWManager.solarsystem_contains_rw_instances",
    "abyssal_content_manager.GenerateAbyssEncounterContent",
    "abyssal_content_manager.GenerateContentFromExistingPlan",
    "abyssal_content_manager.GetContentDetailsForShipLocation",
    "abyssal_content_manager.generate_abyss_npc_spawn_report",
    "inventoryAssetHoldingMgr.is_item_failure_enabled",
    "inventoryAssetHoldingMgr.is_item_validation_enabled",
    "inventoryAssetHoldingMgr.set_item_failure_enabled",
    "inventoryAssetHoldingMgr.set_item_validation_enabled",
    "kiringMgr.ActivateRedeemingCode",
    "kiringMgr.PerformKiringServerSideAuthenticationFromCode",
    "kiringMgr.PlaceKiringOrder",
    "localizationServer.ReloadFSDPickle",
    "milestoneMgr.ClaimRewards",
    "node_graph.qa_start_node",
    "node_graph.qa_start_node_graph",
    "node_graph.qa_stop_active_nodes",
    "node_graph.qa_stop_node",
    "node_graph.qa_stop_node_graph",
    "node_graph.qa_subscribe",
    "node_graph.qa_unsubscribe",
    "posMgr.RunMoonProcessCycleforTower",
];

fn expectations_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.agents/skills/evomu-adversarial-review/references/parity/sources/client_expectations.json")
}

fn seed_comprehensive_world(ctx: &ServerContext) {
    common::pin_clock(ctx);
    common::seed_test_nes_store_catalog(ctx);
    common::seed_test_lp_store_offers(ctx);
    common::seed_test_cosmetics_component_listings(ctx);

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acct) = snap.accounts.get_mut("demo") {
            acct.role = u64::MAX;
        }
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
            ch.war_faction_id = Some(500_001);
            ch.fleet_id = Some(FLEET_ID);
            ch.free_skill_points = 250_000.0;
            ch.loyalty_points.insert(1_000_035, 100_000);
            ch.loyalty_points.insert(1_000_125, 100_000);
            ch.standings.insert(1_000_035, 5.0);
            ch.standings.insert(AGENT_ID, 5.0);
            ch.standings.insert(OTHER_CHAR_ID, 2.0);
        }
        if let Some(corp) = snap.corporations.get_mut(&CORP_ID) {
            corp.alliance_id = Some(ALLIANCE_ID);
            corp.hq_station_id = Some(STATION_ID);
            corp.description = "Demo Corp Description".into();
            corp.url = "https://evomu.local".into();
            corp.applications.insert(
                1,
                CorpApplicationRecord {
                    application_id: 1,
                    character_id: OTHER_CHAR_ID,
                    corporation_id: CORP_ID,
                    message: "Please let me join".into(),
                    created_unix: now,
                    status: "Pending".into(),
                },
            );
        }
        snap.corporations.insert(
            OTHER_CORP_ID,
            CorporationRecord {
                corporation_id: OTHER_CORP_ID,
                name: "Rival Corp".into(),
                ticker: "RIVL".into(),
                ceo_id: OTHER_CHAR_ID,
                alliance_id: None,
                tax_rate: 0.10,
                master_wallet: IskCents::from_isk(50_000_000),
                wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents::from_isk(
                    50_000_000,
                )),
                members: HashSet::from([OTHER_CHAR_ID]),
                member_roles: HashMap::from([(OTHER_CHAR_ID, u64::MAX)]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ALLIANCE_ID,
            AllianceRecord {
                alliance_id: ALLIANCE_ID,
                name: "Demo Alliance".into(),
                short_name: "DMAL".into(),
                executor_corp_id: CORP_ID,
                member_corp_ids: HashSet::from([CORP_ID]),
            },
        );
        snap.wars.insert(
            WAR_ID,
            WarRecord {
                war_id: WAR_ID,
                aggressor_corp_id: CORP_ID,
                defender_corp_id: OTHER_CORP_ID,
                mutual: false,
                started_unix: now.saturating_sub(3600),
                active: true,
            },
        );
        snap.skill_queues.insert(
            CHAR_ID,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 3600.0,
                started_at_unix: Some(now),
            }],
        );
        snap.jump_clones.insert(
            CHAR_ID,
            vec![JumpCloneRecord {
                jump_clone_id: 300_001,
                character_id: CHAR_ID,
                location_id: STATION_ID,
                implants: vec![9899],
            }],
        );
        snap.active_implants.insert(CHAR_ID, vec![9899]);
        snap.saved_fittings.insert(
            400_001,
            SavedFittingRecord {
                fitting_id: 400_001,
                owner_id: CHAR_ID,
                name: "Standard Rifter".into(),
                ship_type_id: 587,
                description: "Test Rifter Fitting".into(),
                items: vec![(2889, InventoryFlag::HiSlot0, 1)],
            },
        );
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 1,
            character_id: CHAR_ID,
            entry_type: "BountyPrize".into(),
            amount: IskCents::from_isk(100_000),
            balance_after: IskCents::from_isk(1_000_100_000),
            reason: "NPC bounty".into(),
        });
        snap.contracts.insert(
            CONTRACT_ID,
            ContractRecord {
                contract_id: CONTRACT_ID,
                issuer_id: CHAR_ID,
                issuer_corp_id: CORP_ID,
                assignee_id: OTHER_CHAR_ID,
                acceptor_id: None,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                start_station_id: STATION_ID,
                end_station_id: STATION_ID,
                price: IskCents::from_isk(100_000),
                reward: IskCents::ZERO,
                collateral: IskCents::ZERO,
                buyout: IskCents::from_isk(200_000),
                current_bid: IskCents::ZERO,
                current_bidder_id: None,
                item_ids: vec![100_000_004],
                requested_items: vec![],
                bid_history: vec![],
                expiration_unix: now + 86_400,
                date_issued_unix: now,
                title: "Seeded Contract".into(),
                for_corp: false,
                num_days: 1,
            },
        );
        snap.industry_jobs.insert(
            800_001,
            IndustryJobRecord {
                job_id: 800_001,
                installer_id: CHAR_ID,
                facility_id: STATION_ID,
                blueprint_item_id: BPO_ITEM_ID,
                blueprint_type_id: 691,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: now.saturating_sub(7200),
            },
        );
        snap.planetary_colonies.insert(
            (CHAR_ID, PLANET_ID),
            PlanetaryColonyRecord {
                planet_id: PLANET_ID,
                owner_id: CHAR_ID,
                solar_system_id: SYSTEM_ID,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2268, 500)]),
                last_extracted_unix: now.saturating_sub(600),
            },
        );
        snap.active_missions.insert(
            (CHAR_ID, 1),
            ActiveMissionRecord {
                mission_id: 1,
                character_id: CHAR_ID,
                agent_id: AGENT_ID,
                solar_system_id: SYSTEM_ID,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: Some(BOOKMARK_ID),
            },
        );
        snap.standing_transactions.entry(CHAR_ID).or_default().push(
            StandingTransactionRecord {
                event_id: 1,
                from_id: 1_000_035,
                to_id: CHAR_ID,
                modification: 0.25,
                event_type_id: 33,
            },
        );
        snap.bookmark_folders.insert(
            FOLDER_ID,
            BookmarkFolderRecord {
                folder_id: FOLDER_ID,
                owner_id: CHAR_ID,
                folder_name: "Personal Locations".into(),
                is_shared: true,
                corporation_id: Some(CORP_ID),
                description: "Seeded Folder".into(),
                ..Default::default()
            },
        );
        snap.bookmarks.insert(
            BOOKMARK_ID,
            BookmarkRecord {
                bookmark_id: BOOKMARK_ID,
                owner_id: CHAR_ID,
                label: "Home Spot".into(),
                solar_system_id: SYSTEM_ID,
                x: 10_000.0,
                y: 20_000.0,
                z: 30_000.0,
                item_id: Some(STATION_ID),
                memo: "Note".into(),
                folder_id: Some(FOLDER_ID),
                is_shared: true,
            },
        );
        snap.mails.push(MailRecord {
            message_id: 1,
            sender_id: OTHER_CHAR_ID,
            recipient_id: CHAR_ID,
            subject: "Welcome".into(),
            body: "Hello Capsuleer".into(),
        });
        snap.mail_labels.entry(CHAR_ID).or_default().push(MailLabelRecord {
            label_id: 1,
            name: "Important".into(),
            color: "#ffffff".into(),
        });
        snap.fleets.insert(
            FLEET_ID,
            FleetRecord {
                fleet_id: FLEET_ID,
                boss_id: CHAR_ID,
                members: vec![CHAR_ID, OTHER_CHAR_ID],
                wings: vec![WING_ID],
                squads: vec![SQUAD_ID],
                free_move: true,
                is_registered: true,
                wing_details: HashMap::from([(
                    WING_ID,
                    FleetWingRecord {
                        wing_id: WING_ID,
                        name: "Wing 1".into(),
                        commander_id: None,
                        squad_ids: vec![SQUAD_ID],
                    },
                )]),
                squad_details: HashMap::from([(
                    SQUAD_ID,
                    FleetSquadRecord {
                        squad_id: SQUAD_ID,
                        wing_id: WING_ID,
                        name: "Squad 1".into(),
                        commander_id: None,
                    },
                )]),
                member_details: HashMap::from([
                    (
                        CHAR_ID,
                        FleetMemberRecord {
                            character_id: CHAR_ID,
                            role: FleetRole::FleetCommander,
                            wing_id: None,
                            squad_id: None,
                            takes_fleet_warp: true,
                            accepts_regroup: true,
                            accepts_conduit_jumps: true,
                            job: 1,
                            role_name: "Fleet Commander".into(),
                            join_time_unix: now,
                        },
                    ),
                    (
                        OTHER_CHAR_ID,
                        FleetMemberRecord {
                            character_id: OTHER_CHAR_ID,
                            role: FleetRole::SquadMember,
                            wing_id: Some(WING_ID),
                            squad_id: Some(SQUAD_ID),
                            takes_fleet_warp: true,
                            accepts_regroup: true,
                            accepts_conduit_jumps: true,
                            job: 0,
                            role_name: "Squad Member".into(),
                            join_time_unix: now,
                        },
                    ),
                ]),
                invites: HashSet::new(),
                motd: "Fleet MOTD".into(),
            },
        );
        snap.structures.insert(
            STRUCTURE_ID,
            UpwellStructureRecord {
                structure_id: STRUCTURE_ID,
                type_id: 35832, // Astrahus
                owner_corp_id: CORP_ID,
                solar_system_id: SYSTEM_ID,
                name: "Jita Trade Citadel".into(),
                position: [100_000.0, 200_000.0, 300_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.killmails.insert(
            1,
            KillmailRecord {
                kill_id: 1,
                victim_character_id: OTHER_CHAR_ID,
                attacker_character_id: CHAR_ID,
                solar_system_id: SYSTEM_ID,
                ship_type_id: 587,
                total_value_isk: 500_000.0,
                killed_unix: now.saturating_sub(600),
                victim_corp_id: OTHER_CORP_ID,
                victim_alliance_id: None,
                attacker_corp_id: CORP_ID,
                attacker_alliance_id: Some(ALLIANCE_ID),
            },
        );
        snap.kill_rights.insert(
            1,
            KillRightRecord {
                kill_right_id: 1,
                owner_id: CHAR_ID,
                target_id: OTHER_CHAR_ID,
                activation_fee: IskCents::from_isk(1_000_000),
                is_active_suspect: false,
            },
        );
        snap.owner_notes.insert(
            1,
            OwnerNoteRecord {
                note_id: 1,
                character_id: CHAR_ID,
                item_id: CHAR_ID,
                label: "N:My Note".into(),
                note: "Note body".into(),
                timestamp_unix: now,
            },
        );
    });
}

fn unwrap_envelope(rep: &PyRep, preserve_cached_obj: bool) -> PyRep {
    match rep {
        PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
            unwrap_envelope(inner, preserve_cached_obj)
        }
        PyRep::ChecksumedStream { body, .. } => unwrap_envelope(body, preserve_cached_obj),
        PyRep::Object { type_name, args }
            if type_name == "objectCaching.CachedMethodCallResult"
                || type_name == "util.CachedMethodCallResult"
                || type_name == "utillib.CachedMethodCallResult" =>
        {
            if let PyRep::Tuple(items) = args.as_ref() {
                if let Some(sub) = items.get(1) {
                    return unwrap_envelope(sub, preserve_cached_obj);
                }
            }
            rep.clone()
        }
        PyRep::Object { type_name, args }
            if !preserve_cached_obj
                && (type_name == "objectCaching.CachedObject" || type_name == "util.CachedObject") =>
        {
            if let PyRep::Tuple(items) = args.as_ref() {
                if let Some(PyRep::Buffer(bytes) | PyRep::Bytes(bytes)) = items.get(4) {
                    if let Ok(decoded) = eve_proto::marshal::decode(bytes) {
                        return unwrap_envelope(&decoded, preserve_cached_obj);
                    }
                }
            }
            rep.clone()
        }
        _ => rep.clone(),
    }
}

fn rowset_row_to_keyval(columns: &[String], row: &[PyRep]) -> PyRep {
    let mut map = HashMap::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        map.insert(col.clone(), row.get(i).cloned().unwrap_or(PyRep::None));
    }
    PyRep::KeyVal(map)
}

fn crowset_descriptor_columns(rep: &PyRep) -> Option<Vec<String>> {
    if let PyRep::ObjectEx { header, .. } = rep {
        if let PyRep::Tuple(items) = header.as_ref() {
            if let Some(PyRep::Tuple(args)) = items.first() {
                if let Some(desc_rep) = args.get(1).and_then(|t| {
                    if let PyRep::Tuple(inner) = t {
                        inner.first()
                    } else {
                        Some(t)
                    }
                }) {
                    if let Some(defs) = eve_proto::extract_dbrow_header_col_defs(desc_rep) {
                        return Some(defs.into_iter().map(|(n, _)| n).collect());
                    }
                }
            }
        }
    }
    None
}

fn parse_path_segments(path: &str) -> Vec<String> {
    if path == "$" || path.is_empty() {
        return Vec::new();
    }
    let mut segs = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(".keys()") {
            segs.push(".keys()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix(".values()") {
            segs.push(".values()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix(".items()") {
            segs.push(".items()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix("[]") {
            segs.push("[]".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix("[*]") {
            segs.push("[*]".to_string());
            rest = after;
        } else if rest.starts_with('[') {
            if let Some(end) = rest.find(']') {
                segs.push(rest[..=end].to_string());
                rest = &rest[end + 1..];
            } else {
                break;
            }
        } else if let Some(after) = rest.strip_prefix('.') {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            segs.push(format!(".{}", &after[..end]));
            rest = &after[end..];
        } else {
            break;
        }
    }
    segs
}

fn step_segment(val: &PyRep, seg: &str) -> Vec<PyRep> {
    let val = unwrap_envelope(val, false);
    if matches!(val, PyRep::None) {
        return Vec::new();
    }
    match seg {
        "[]" => match &val {
            PyRep::List(items) | PyRep::Tuple(items) | PyRep::Set(items) | PyRep::PySet(items) => {
                items.clone()
            }
            PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => rows
                .iter()
                .map(|r| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::IndexRowset { columns, items, .. } => items
                .iter()
                .map(|(_, r)| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::Dict(entries) => {
                if entries.first().is_some_and(|(_, v)| {
                    matches!(
                        v,
                        PyRep::KeyVal(_)
                            | PyRep::DBRow { .. }
                            | PyRep::Object { .. }
                            | PyRep::ObjectEx { .. }
                    )
                }) {
                    entries.iter().map(|(_, v)| v.clone()).collect()
                } else {
                    entries.iter().map(|(k, _)| k.clone()).collect()
                }
            }
            PyRep::ObjectEx { list, dict, .. } => {
                if val.is_crowset() {
                    list.clone()
                } else if val.is_cindexed_rowset() {
                    dict.iter().map(|(k, _)| k.clone()).collect()
                } else if !list.is_empty() {
                    list.clone()
                } else {
                    dict.iter().map(|(k, _)| k.clone()).collect()
                }
            }
            _ => Vec::new(),
        },
        "[*]" => match &val {
            PyRep::Dict(entries) => entries.iter().map(|(_, v)| v.clone()).collect(),
            PyRep::KeyVal(map) => map.values().cloned().collect(),
            PyRep::IndexRowset { columns, items, .. } => items
                .iter()
                .map(|(_, r)| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::List(items) | PyRep::Tuple(items) => items.clone(),
            PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => rows
                .iter()
                .map(|r| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::ObjectEx { list, dict, .. } => {
                if val.is_cindexed_rowset() || !dict.is_empty() {
                    dict.iter().map(|(_, v)| v.clone()).collect()
                } else {
                    list.clone()
                }
            }
            _ => Vec::new(),
        },
        ".keys()" => match &val {
            PyRep::Dict(entries) => vec![PyRep::List(entries.iter().map(|(k, _)| k.clone()).collect())],
            PyRep::KeyVal(map) => {
                vec![PyRep::List(map.keys().cloned().map(PyRep::String).collect())]
            }
            PyRep::IndexRowset { items, .. } => {
                vec![PyRep::List(items.iter().map(|(k, _)| k.clone()).collect())]
            }
            PyRep::ObjectEx { dict, .. } => {
                vec![PyRep::List(dict.iter().map(|(k, _)| k.clone()).collect())]
            }
            _ => Vec::new(),
        },
        ".values()" => match &val {
            PyRep::Dict(entries) => vec![PyRep::List(entries.iter().map(|(_, v)| v.clone()).collect())],
            PyRep::KeyVal(map) => vec![PyRep::List(map.values().cloned().collect())],
            PyRep::IndexRowset { columns, items, .. } => vec![PyRep::List(
                items
                    .iter()
                    .map(|(_, r)| rowset_row_to_keyval(columns, r))
                    .collect(),
            )],
            PyRep::ObjectEx { dict, .. } => {
                vec![PyRep::List(dict.iter().map(|(_, v)| v.clone()).collect())]
            }
            _ => Vec::new(),
        },
        ".items()" => match &val {
            PyRep::Dict(entries) => vec![PyRep::List(
                entries
                    .iter()
                    .map(|(k, v)| PyRep::Tuple(vec![k.clone(), v.clone()]))
                    .collect(),
            )],
            PyRep::KeyVal(map) => vec![PyRep::List(
                map.iter()
                    .map(|(k, v)| PyRep::Tuple(vec![PyRep::String(k.clone()), v.clone()]))
                    .collect(),
            )],
            PyRep::IndexRowset { columns, items, .. } => vec![PyRep::List(
                items
                    .iter()
                    .map(|(k, r)| PyRep::Tuple(vec![k.clone(), rowset_row_to_keyval(columns, r)]))
                    .collect(),
            )],
            PyRep::ObjectEx { dict, .. } => vec![PyRep::List(
                dict.iter()
                    .map(|(k, v)| PyRep::Tuple(vec![k.clone(), v.clone()]))
                    .collect(),
            )],
            _ => Vec::new(),
        },
        _ if seg.starts_with('.') => {
            let attr = &seg[1..];
            if let Some(v) = get_attr(&val, attr) {
                vec![v]
            } else {
                Vec::new()
            }
        }
        _ if seg.starts_with('[') && seg.ends_with(']') => {
            let inner = &seg[1..seg.len() - 1];
            if let Some(str_key) = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                if let Some(v) = get_subscript_str(&val, str_key) {
                    vec![v]
                } else {
                    Vec::new()
                }
            } else if let Ok(idx) = inner.parse::<isize>() {
                if let Some(v) = get_subscript_int(&val, idx) {
                    vec![v]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn get_attr(val: &PyRep, attr: &str) -> Option<PyRep> {
    match val {
        PyRep::KeyVal(map) => {
            if attr == "__dict__" {
                return Some(val.clone());
            }
            map.get(attr).cloned()
        }
        PyRep::DBRow {
            header, columns, ..
        } => {
            if attr == "__header__" || attr == "header" {
                return Some((**header).clone());
            }
            if attr == "__keys__" || attr == "__columns__" || attr == "columns" {
                return Some(PyRep::List(
                    columns
                        .iter()
                        .map(|(k, _)| PyRep::String(k.clone()))
                        .collect(),
                ));
            }
            if attr == "line" {
                return Some(PyRep::List(columns.iter().map(|(_, v)| v.clone()).collect()));
            }
            if let Some((_, v)) = columns.iter().find(|(k, _)| k == attr) {
                return Some(v.clone());
            }
            // C++ `blue.DBRow` virtual properties on `invItem` rows (`DBRowDescriptor::inv_item_header`)
            if attr == "flag" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "flagID") {
                    return Some(v.clone());
                }
            }
            if attr == "stacksize" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "quantity") {
                    let q = v.as_i64().unwrap_or(1);
                    return Some(PyRep::Integer(q.max(1)));
                }
            }
            if attr == "singleton" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "quantity") {
                    let q = v.as_i64().unwrap_or(1);
                    return Some(PyRep::Integer(if q < 0 { -q } else { 0 }));
                }
            }
            None
        }
        PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => match attr {
            "columns" | "__columns__" | "header" => Some(PyRep::List(
                columns.iter().cloned().map(PyRep::String).collect(),
            )),
            "lines" => Some(PyRep::List(
                rows.iter().cloned().map(PyRep::List).collect(),
            )),
            _ => rows
                .first()
                .and_then(|r| rowset_row_to_keyval(columns, r).dict_get(attr).cloned()),
        },
        PyRep::IndexRowset {
            key_column,
            columns,
            items,
        } => match attr {
            "columns" | "__columns__" | "header" => Some(PyRep::List(
                columns.iter().cloned().map(PyRep::String).collect(),
            )),
            "key" => Some(PyRep::String(key_column.clone())),
            "items" => Some(PyRep::Dict(
                items
                    .iter()
                    .map(|(k, r)| (k.clone(), rowset_row_to_keyval(columns, r)))
                    .collect(),
            )),
            _ => None,
        },
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let (Some(PyRep::List(cols)), Some(PyRep::List(vals))) =
                        (parts.first(), parts.get(1))
                    {
                        if attr == "header" || attr == "columns" || attr == "__columns__" {
                            return Some(PyRep::List(cols.clone()));
                        }
                        if attr == "line" {
                            return Some(PyRep::List(vals.clone()));
                        }
                        for (i, c) in cols.iter().enumerate() {
                            if c.as_str() == Some(attr) {
                                return Some(vals.get(i).cloned().unwrap_or(PyRep::None));
                            }
                        }
                    }
                }
            }
            get_attr(args, attr)
        }
        PyRep::ObjectEx { header, dict, .. } => {
            if attr == "header" || attr == "__header__" {
                return Some((**header).clone());
            }
            dict.iter()
                .find(|(k, _)| k.as_str() == Some(attr))
                .map(|(_, v)| v.clone())
        }
        _ => None,
    }
}

fn get_subscript_str(val: &PyRep, key: &str) -> Option<PyRep> {
    match val {
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone()),
        PyRep::DBRow { columns, .. } => columns
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone()),
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let (Some(PyRep::List(cols)), Some(PyRep::List(vals))) =
                        (parts.first(), parts.get(1))
                    {
                        if key == "header" || key == "columns" {
                            return Some(PyRep::List(cols.clone()));
                        }
                        if key == "line" {
                            return Some(PyRep::List(vals.clone()));
                        }
                        for (i, c) in cols.iter().enumerate() {
                            if c.as_str() == Some(key) {
                                return Some(vals.get(i).cloned().unwrap_or(PyRep::None));
                            }
                        }
                    }
                }
            }
            get_subscript_str(args, key)
        }
        PyRep::ObjectEx { dict, .. } => dict
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone()),
        _ => None,
    }
}

fn get_subscript_int(val: &PyRep, idx: isize) -> Option<PyRep> {
    let resolve_idx = |len: usize| -> Option<usize> {
        if idx >= 0 {
            let u = idx as usize;
            (u < len).then_some(u)
        } else {
            let back = (-idx) as usize;
            (back <= len).then(|| len - back)
        }
    };
    match val {
        PyRep::Tuple(items) | PyRep::List(items) => {
            resolve_idx(items.len()).map(|i| items[i].clone())
        }
        PyRep::Rowset { columns, rows } => resolve_idx(rows.len())
            .map(|i| rowset_row_to_keyval(columns, &rows[i])),
        PyRep::TupleSet { rows, .. } => {
            if idx == 0 || idx == 1 {
                // Note: TupleSet wire format is `(columns, rows)` when unpacked as 2-tuple,
                // OR indexed by row if converted to Rowset.
                resolve_idx(rows.len()).map(|i| PyRep::List(rows[i].clone()))
            } else {
                resolve_idx(rows.len()).map(|i| PyRep::List(rows[i].clone()))
            }
        }
        PyRep::DBRow { columns, .. } => {
            resolve_idx(columns.len()).map(|i| columns[i].1.clone())
        }
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let Some(PyRep::List(vals)) = parts.get(1) {
                        return resolve_idx(vals.len()).map(|i| vals[i].clone());
                    }
                } else if let Some(PyRep::List(vals)) = get_subscript_str(args, "line") {
                    return resolve_idx(vals.len()).map(|i| vals[i].clone());
                }
            }
            get_subscript_int(args, idx)
        }
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(idx as i64))
            .map(|(_, v)| v.clone()),
        PyRep::IndexRowset { columns, items, .. } => items
            .iter()
            .find(|(k, _)| k.as_i64() == Some(idx as i64))
            .map(|(_, r)| rowset_row_to_keyval(columns, r)),
        PyRep::ObjectEx { list, dict, .. } => {
            if val.is_crowset() || !list.is_empty() {
                resolve_idx(list.len()).map(|i| list[i].clone())
            } else {
                dict.iter()
                    .find(|(k, _)| k.as_i64() == Some(idx as i64))
                    .map(|(_, v)| v.clone())
            }
        }
        _ => None,
    }
}

fn resolve_path(root: &PyRep, path: &str) -> Vec<PyRep> {
    let segs = parse_path_segments(path);
    let mut current = vec![root.clone()];
    for seg in segs {
        let mut next = Vec::new();
        for v in &current {
            next.extend(step_segment(v, &seg));
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

fn has_column_in_collection(val: &PyRep, col: &str) -> bool {
    let val = unwrap_envelope(val, false);
    let matches_col = |cols: &[String]| -> bool {
        cols.iter().any(|c| c == col)
            || (col == "flag" && cols.iter().any(|c| c == "flagID"))
            || ((col == "stacksize" || col == "singleton") && cols.iter().any(|c| c == "quantity"))
    };
    match &val {
        PyRep::Rowset { columns, .. }
        | PyRep::IndexRowset { columns, .. }
        | PyRep::TupleSet { columns, .. } => matches_col(columns),
        PyRep::ObjectEx { list, .. } if val.is_crowset() || val.is_cindexed_rowset() => {
            if let Some(cols) = crowset_descriptor_columns(&val) {
                return matches_col(&cols);
            }
            list.first().is_some_and(|r| get_attr(r, col).is_some())
        }
        _ => false,
    }
}

/// Validates a `PyRep` response against the client's `reply_shape` expectations.
/// Returns a list of human-readable contract violations (empty if compliant).
fn validate_reply_shape(
    rpc_name: &str,
    raw_reply: &PyRep,
    reply_shape: &serde_json::Map<String, Value>,
) -> Vec<String> {
    let mut violations = Vec::new();
    let root_ops: Vec<&str> = reply_shape
        .get("$")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();

    let preserve_cached = root_ops.contains(&"attr:GetCachedObject");
    let root = unwrap_envelope(raw_reply, preserve_cached);

    // Check if root is only discarded (`["discard"]`)
    if reply_shape.len() == 1 && root_ops.iter().all(|&o| o == "discard") {
        return violations;
    }

    for (path, ops_val) in reply_shape {
        let Some(ops_arr) = ops_val.as_array() else {
            continue;
        };
        let ops: Vec<&str> = ops_arr.iter().filter_map(|v| v.as_str()).collect();
        let values = resolve_path(&root, path);

        // If path is `$` and `unpack:N` is present without null-check guards, `None` is a violation
        let unpack_arities: Vec<usize> = ops
            .iter()
            .filter_map(|o| o.strip_prefix("unpack:")?.parse::<usize>().ok())
            .collect();
        let has_null_guard = ops.iter().any(|o| {
            matches!(
                *o,
                "truthy" | "compareisNone" | "compareisnull" | "compareis_notnull" | "compare==null" | "compare!=null"
            )
        });

        if path == "$" && !unpack_arities.is_empty() && !has_null_guard && matches!(root, PyRep::None)
        {
            violations.push(format!(
                "{rpc_name}: path `$` returned None, but client unconditionally unpacks {unpack_arities:?}"
            ));
            continue;
        }

        for val in &values {
            let val = unwrap_envelope(val, preserve_cached);
            if matches!(val, PyRep::None) {
                continue;
            }

            // 1. Validate `unpack:N`
            if !unpack_arities.is_empty() {
                let actual_len = match &val {
                    PyRep::Tuple(items) | PyRep::List(items) | PyRep::Set(items) | PyRep::PySet(items) => {
                        Some(items.len())
                    }
                    PyRep::TupleSet { .. } => Some(2),
                    PyRep::DBRow { columns, .. } => Some(columns.len()),
                    PyRep::Object { type_name, args }
                        if type_name.ends_with(".Row") || type_name == "Row" =>
                    {
                        if let PyRep::Tuple(parts) = args.as_ref() {
                            parts.get(1).and_then(|p| match p {
                                PyRep::List(l) | PyRep::Tuple(l) => Some(l.len()),
                                _ => None,
                            })
                        } else if let Some(PyRep::List(vals)) = get_subscript_str(args, "line") {
                            Some(vals.len())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                match actual_len {
                    None => violations.push(format!(
                        "{rpc_name}: path `{path}` expected unpackable sequence {unpack_arities:?}, got non-sequence {val:?}"
                    )),
                    Some(n) if !unpack_arities.contains(&n) => {
                        // Allow variable-length list unpacking only when `slice` is also used
                        let has_slice = ops.iter().any(|o| o.starts_with("slice"));
                        if !has_slice {
                            violations.push(format!(
                                "{rpc_name}: path `{path}` expected tuple/list arity in {unpack_arities:?}, got len {n} ({val:?})"
                            ));
                        }
                    }
                    _ => {}
                }
            }

            // 1b. Validate `iter` at path when the client actually iterates elements (`[]` child path)
            // and is not guarded by `isinstance:?`. (Avoids false positives from `max(a, b)`/`min(a, b)` on scalars.)
            let child_iter_path = if path == "$" {
                "[]".to_string()
            } else {
                format!("{path}[]")
            };
            let has_isinstance_guard = ops.iter().any(|o| o.starts_with("isinstance:"));
            if ops.contains(&"iter")
                && reply_shape.contains_key(&child_iter_path)
                && !has_isinstance_guard
            {
                let is_iterable = matches!(
                    val,
                    PyRep::List(_)
                        | PyRep::Tuple(_)
                        | PyRep::Set(_)
                        | PyRep::PySet(_)
                        | PyRep::Dict(_)
                        | PyRep::Rowset { .. }
                        | PyRep::IndexRowset { .. }
                        | PyRep::TupleSet { .. }
                        | PyRep::DBRow { .. }
                        | PyRep::ObjectEx { .. }
                        | PyRep::Object { .. }
                );
                if !is_iterable {
                    violations.push(format!(
                        "{rpc_name}: path `{path}` expected iterable (`iter`), got non-iterable {val:?}"
                    ));
                }
            }

            // 2. Validate `rowset_index["col"]` and `rowset_filter["col"]`
            for op in &ops {
                if let Some(col_raw) = op
                    .strip_prefix("rowset_index[")
                    .or_else(|| op.strip_prefix("rowset_filter["))
                    .and_then(|s| s.strip_suffix(']'))
                {
                    let col = col_raw.trim_matches('"');
                    if !has_column_in_collection(&val, col) {
                        violations.push(format!(
                            "{rpc_name}: path `{path}` requires Rowset/CRowset with column `{col}` ({op}), got {val:?}"
                        ));
                    }
                }
            }

            // 3. Validate `attr:<field>`
            for op in &ops {
                let Some(field) = op.strip_prefix("attr:") else {
                    continue;
                };
                // Skip Python collection/string method names and internal bound-object methods
                if matches!(
                    field,
                    "append"
                        | "extend"
                        | "insert"
                        | "remove"
                        | "pop"
                        | "sort"
                        | "reverse"
                        | "count"
                        | "index"
                        | "update"
                        | "clear"
                        | "setdefault"
                        | "add"
                        | "discard"
                        | "union"
                        | "intersection"
                        | "difference"
                        | "split"
                        | "strip"
                        | "lower"
                        | "upper"
                        | "replace"
                        | "startswith"
                        | "endswith"
                        | "format"
                        | "join"
                        | "encode"
                        | "decode"
                        | "iterkeys"
                        | "itervalues"
                        | "iteritems"
                        | "has_key"
                        | "viewkeys"
                        | "viewvalues"
                        | "viewitems"
                        | "__dict__"
                        | "__keys__"
                        | "__columns__"
                        | "__header__"
                        | "header"
                        | "columns"
                        | "lines"
                        | "line"
                        | "clear_memoized"
                        | "remove_from_cache"
                        | "prime_cache_result"
                        | "virtual"
                        | "GetCachedObject"
                        | "GetNextPendingChangeTime"
                        | "IsFriendlyFireLegalAtTime"
                        | "GetAllBehaviorEntriesByKey"
                        | "List"
                ) {
                    if matches!(field, "append" | "extend" | "insert" | "sort" | "reverse")
                        && !matches!(val, PyRep::List(_) | PyRep::Rowset { .. })
                        && !val.is_crowset()
                    {
                        violations.push(format!(
                            "{rpc_name}: path `{path}` calls list method `.{field}()`, but server returned non-List {val:?}"
                        ));
                    }
                    continue;
                }

                // Skip if guarded by `hasattr["field"]` or `getattr["field"]`
                let has_guard = ops.iter().any(|o| {
                    *o == format!("hasattr[\"{field}\"]")
                        || *o == format!("getattr[\"{field}\"]")
                        || o.starts_with("isinstance:")
                });
                if has_guard {
                    continue;
                }

                // A raw `PyRep::Dict` raises `AttributeError` in Python on `.field` access!
                if matches!(val, PyRep::Dict(_)) {
                    violations.push(format!(
                        "{rpc_name}: path `{path}` accesses `.{field}` (`attr:{field}`), but server returned raw `PyRep::Dict` instead of `KeyVal`/`DBRow`/`Rowset`"
                    ));
                    continue;
                }

                // If `val` is a Rowset/CRowset at `$` and client accesses `.<field>` directly (e.g. `corpmgr.GetCorporations`),
                // check either direct attribute or column presence.
                if matches!(
                    val,
                    PyRep::Rowset { .. } | PyRep::IndexRowset { .. } | PyRep::TupleSet { .. }
                ) || val.is_crowset()
                    || val.is_cindexed_rowset()
                {
                    if !has_column_in_collection(&val, field) && get_attr(&val, field).is_none() {
                        violations.push(format!(
                            "{rpc_name}: path `{path}` accesses `.{field}` on Rowset/CRowset, but column `{field}` is missing from header"
                        ));
                    }
                    continue;
                }

                // If `val` is a `SubStruct` (bound object OID), skip attribute check
                if matches!(val, PyRep::SubStruct(_)) {
                    continue;
                }

                if get_attr(&val, field).is_none() {
                    violations.push(format!(
                        "{rpc_name}: path `{path}` missing required attribute `.{field}` in {val:?}"
                    ));
                }
            }
        }
    }

    violations
}

fn parse_const_repr(s: &str) -> PyRep {
    if s == "null" || s == "None" {
        PyRep::None
    } else if s == "True" || s == "true" {
        PyRep::Bool(true)
    } else if s == "False" || s == "false" {
        PyRep::Bool(false)
    } else if s == "()" {
        PyRep::Tuple(Vec::new())
    } else if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        PyRep::String(inner.to_string())
    } else if let Ok(i) = s.parse::<i64>() {
        PyRep::Integer(i)
    } else if let Ok(f) = s.parse::<f64>() {
        PyRep::Real(f)
    } else {
        PyRep::String(s.to_string())
    }
}

fn default_domain_arg(service: &str, method: &str, idx: usize) -> PyRep {
    let m_low = method.to_ascii_lowercase();
    if m_low.contains("items") || m_low.contains("ids") || m_low.starts_with("multi") {
        if idx == 0 {
            return PyRep::List(vec![PyRep::Integer(HANGAR_ITEM_ID)]);
        }
    }
    match service {
        "charMgr" | "charUnboundMgr" | "skillMgr2" | "onlineStatus" | "paperDollServer"
        | "jumpCloneSvc" | "certificateMgr" | "charFittingMgr" | "characterEnergyMgr"
        | "characterMiningLedger" => match idx {
            0 => PyRep::Integer(CHAR_ID),
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "corpRegistry" | "corpmgr" | "corporationSvc" | "corpStationMgr" | "corpBookmarkMgr"
        | "corpFittingMgr" | "corpMiningLedger" | "corpRecProxy" | "voteManager" => match idx {
            0 => PyRep::Integer(CORP_ID),
            1 => PyRep::Integer(CHAR_ID),
            _ => PyRep::Integer(1),
        },
        "allianceRegistry" | "allianceFittingMgr" => match idx {
            0 => PyRep::Integer(ALLIANCE_ID),
            1 => PyRep::Integer(CORP_ID),
            _ => PyRep::Integer(1),
        },
        "warRegistry" | "warStatisticMgr" | "warsInfoMgr" | "mutualWarInviteMgr"
        | "peaceTreatyMgr" => match idx {
            0 => PyRep::Integer(WAR_ID),
            1 => PyRep::Integer(OTHER_CORP_ID),
            _ => PyRep::Integer(1),
        },
        "fleetMgr" | "fleetObjectHandler" | "fleetProxy" => match idx {
            0 => PyRep::Integer(OTHER_CHAR_ID),
            1 => PyRep::Integer(WING_ID),
            2 => PyRep::Integer(SQUAD_ID),
            _ => PyRep::Integer(1),
        },
        "structure"
        | "structureAssetSafety"
        | "structureCargoDelivery"
        | "structureCompressionMgr"
        | "structureControl"
        | "structureCynoBeaconMgr"
        | "structureDeliveries"
        | "structureDeployment"
        | "structureDirectory"
        | "structureDocking"
        | "structureGuests"
        | "structureHangarViewMgr"
        | "structureJumpBridgeMgr"
        | "structureSettings"
        | "structureVulnerability" => match idx {
            0 => PyRep::Integer(STRUCTURE_ID),
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "station" | "stationSvc" | "repairSvc" | "reprocessingSvc" | "insuranceSvc" => match idx {
            0 => PyRep::Integer(SHIP_ID),
            1 => PyRep::Integer(STATION_ID),
            _ => PyRep::Integer(1),
        },
        "invbroker" | "dogmaIM" | "ship" | "repackagingSvc" | "itemLocking" => match idx {
            0 if service == "invbroker" && method == "GetInventory" => PyRep::Integer(10_004),
            0 => PyRep::Integer(MODULE_ID),
            1 => PyRep::Integer(4), // Hangar flag
            _ => PyRep::Integer(1),
        },
        "marketProxy" => match idx {
            0 => PyRep::Integer(34), // Tritanium typeID
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "contractProxy" => match idx {
            0 => PyRep::Integer(CONTRACT_ID),
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "planetMgr" | "planetBaseBroker" | "planetOrbitalRegistryBroker" | "colonyResourcesMgr" => {
            match idx {
                0 => PyRep::Integer(PLANET_ID),
                1 => PyRep::Integer(SYSTEM_ID as i64),
                _ => PyRep::Integer(1),
            }
        }
        "agentMgr" | "missionMgr" | "missionTrackerMgr" => match idx {
            0 => PyRep::Integer(AGENT_ID),
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "bookmark" | "accessGroupBookmarkMgr" => match idx {
            0 => PyRep::Integer(FOLDER_ID),
            1 => PyRep::Integer(BOOKMARK_ID),
            _ => PyRep::Integer(1),
        },
        "map" | "sovMgr" | "devIndexManager" | "fwWarzoneSolarsystem" | "insurgencySolarsystem"
        | "solarsystemInterferenceMgr" | "essMgr" | "dynamicBountyMgr"
        | "dynamicResourceCacheMgr" => match idx {
            0 => PyRep::Integer(SYSTEM_ID as i64),
            1 => PyRep::Integer(1),
            _ => PyRep::Integer(1),
        },
        "LPStoreMgr" | "LPSvc" => match idx {
            0 => PyRep::Integer(1_000_035),
            1 => PyRep::Integer(1001),
            _ => PyRep::Integer(1),
        },
        _ => PyRep::Integer(1),
    }
}

fn build_candidate_arg_sets(
    service: &str,
    method: &str,
    arity: usize,
    arg_consts: &[Option<String>],
) -> Vec<Vec<PyRep>> {
    if service == "miningScanMgr" && method == "perform_scan" {
        return vec![vec![PyRep::Integer(MODULE_ID)]];
    }
    let mut base = Vec::with_capacity(arity);
    for i in 0..arity {
        if let Some(Some(c)) = arg_consts.get(i) {
            base.push(parse_const_repr(c));
        } else {
            base.push(default_domain_arg(service, method, i));
        }
    }
    let mut candidates = vec![base.clone()];
    if arity > 0 && arg_consts.first().and_then(|x| x.as_ref()).is_none() {
        for alt0 in [
            PyRep::Integer(CHAR_ID),
            PyRep::Integer(SHIP_ID),
            PyRep::Integer(HANGAR_ITEM_ID),
            PyRep::Integer(STATION_ID),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(HANGAR_ITEM_ID)]),
        ] {
            let mut v = base.clone();
            v[0] = alt0;
            if !candidates.contains(&v) {
                candidates.push(v);
            }
        }
    }
    candidates
}

fn bind_for_service(ctx: &ServerContext, sess: &SessionState, service: &str) -> Option<u64> {
    let obj_id = match service {
        "populationCap" | "skillMgr2" | "charMgr" | "jumpCloneSvc" | "crimewatch" => CHAR_ID,
        "beyonce" | "scanMgr" | "devIndexManager" | "posMgr" | "keeper" => SYSTEM_ID as i64,
        "dogmaIM" | "ship" => sess.ship_id.unwrap_or(SHIP_ID),
        "invbroker" | "station" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => sess
            .station_id
            .or(sess.structure_id)
            .unwrap_or(STATION_ID),
        "planetMgr" => PLANET_ID,
        "fleetObjectHandler" => FLEET_ID,
        "corpRegistry" | "corpmgr" => CORP_ID,
        "allianceRegistry" => ALLIANCE_ID,
        "warRegistry" | "warStatisticMgr" => WAR_ID,
        "contractProxy" => CONTRACT_ID,
        "structure" => STRUCTURE_ID,
        "agentMgr" => AGENT_ID,
        "trademgr" | "voucher" | "i2" => 1,
        _ => return None,
    };
    ctx.bind_object(sess, service, obj_id).ok()
}

fn restore_session_state(ctx: &ServerContext, sess: &mut SessionState, station_id: Option<i64>) {
    sess.user_id = Some(1);
    sess.character_id = Some(CHAR_ID);
    sess.role = u64::MAX;
    sess.corporation_id = Some(CORP_ID);
    sess.alliance_id = Some(ALLIANCE_ID);
    sess.station_id = station_id;
    sess.structure_id = None;
    sess.solar_system_id = Some(SYSTEM_ID);
    sess.constellation_id = Some(20_000_020);
    sess.region_id = Some(10_000_002);
    sess.ship_id = Some(SHIP_ID);
    sess.fleet_id = Some(FLEET_ID);
    if ctx
        .cluster_state
        .read()
        .is_ok_and(|cs| cs.shutdown_in_progress)
    {
        if let Ok(mut cs) = ctx.cluster_state.write() {
            cs.shutdown_in_progress = false;
        }
    }
    let needs_db_sync = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&CHAR_ID).is_some_and(|ch| {
            ch.station_id != station_id
                || ch.structure_id.is_some()
                || ch.solar_system_id != SYSTEM_ID
                || ch.active_ship_id != SHIP_ID
                || ch.corporation_id != CORP_ID
                || ch.alliance_id != Some(ALLIANCE_ID)
                || ch.fleet_id != Some(FLEET_ID)
        })
    });
    if needs_db_sync {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.station_id = station_id;
                ch.structure_id = None;
                ch.solar_system_id = SYSTEM_ID;
                ch.active_ship_id = SHIP_ID;
                ch.corporation_id = CORP_ID;
                ch.alliance_id = Some(ALLIANCE_ID);
                ch.fleet_id = Some(FLEET_ID);
            }
        });
    }
}

fn restore_session_docked(ctx: &ServerContext, sess: &mut SessionState) {
    restore_session_state(ctx, sess, Some(STATION_ID));
}

fn restore_session_in_space(ctx: &ServerContext, sess: &mut SessionState) {
    restore_session_state(ctx, sess, None);
}

fn is_destructive_rpc(service: &str, method: &str) -> bool {
    let m = method.to_ascii_lowercase();
    matches!(
        (service, method),
        ("authentication", "Logout")
            | ("userSvc", "UserLogOffCharacter")
            | ("charUnboundMgr", "DeleteCharacter" | "PrepareCharacterForDelete")
            | ("machoNet", "TerminateAllClients" | "GracefulShutdown")
            | ("sessionMgr", "EndAllGameSessions")
            | ("ship", "Eject" | "SelfDestruct")
            | ("stationSvc", "Undock")
            | ("structureDocking", "Undock")
    ) || m.starts_with("delete")
        || m.starts_with("remove")
        || m.starts_with("destroy")
        || m.starts_with("leave")
        || m.starts_with("kick")
        || m.starts_with("abandon")
        || m.starts_with("reprocess")
        || m.starts_with("repackage")
        || m.starts_with("unanchor")
        || m.starts_with("abort")
        || m.starts_with("cancel")
        || m.starts_with("trash")
        || m.starts_with("extract")
}

#[tokio::test]
async fn test_all_client_rpc_call_sites_and_reply_shape_contracts() {
    let raw = std::fs::read_to_string(expectations_path()).expect("read client_expectations.json");
    let root: Value = serde_json::from_str(&raw).expect("parse client_expectations.json");
    let rpc_map = root["rpc"].as_object().expect("rpc map");

    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    seed_comprehensive_world(&ctx);

    let mut sess = SessionState::new(77_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Spawn the active ship ball in Jita so space/destiny/dogma RPCs work when undocked
    let sys_handle = ctx.systems.get_or_spawn(SYSTEM_ID);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_ID,
            type_id: 587,
            position: glam::DVec3::new(10_000.0, 20_000.0, 30_000.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_350_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Partition into non-destructive (read/query) first, then destructive/mutating second
    let mut non_destructive = Vec::new();
    let mut destructive = Vec::new();
    for (rpc_key, rpc_val) in rpc_map {
        if CLIENT_ONLY_PSEUDO_RPCS.contains(&rpc_key.as_str())
            || INTENTIONALLY_UNIMPLEMENTED_RPCS.contains(&rpc_key.as_str())
        {
            continue;
        }
        let Some((svc, meth)) = rpc_key.split_once('.') else {
            continue;
        };
        if is_destructive_rpc(svc, meth) {
            destructive.push((rpc_key.clone(), svc.to_string(), meth.to_string(), rpc_val));
        } else {
            non_destructive.push((rpc_key.clone(), svc.to_string(), meth.to_string(), rpc_val));
        }
    }

    let mut saw_unimplemented_rpcs = BTreeSet::new();
    let mut shape_violations = Vec::new();
    let mut succeeded_rpcs = BTreeSet::new();
    let mut all_emitted_notifs: Vec<(String, Vec<PyRep>)> = Vec::new();

    struct PendingSig {
        rpc_key: String,
        service: String,
        method: String,
        kw: HashMap<String, PyRep>,
        candidates: Vec<Vec<PyRep>>,
        effective_shape: serde_json::Map<String, Value>,
    }

    let mut pending_sigs = Vec::new();
    for (rpc_key, service, method, rpc_val) in non_destructive.into_iter().chain(destructive) {
        let reply_shape = rpc_val["reply_shape"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let sites = rpc_val["call_sites"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        // Deduplicate call-site signatures by (args_len, kwargs_keys, arg_consts)
        let mut unique_sigs: BTreeMap<
            (usize, Vec<String>, Vec<Option<String>>),
            Vec<String>,
        > = BTreeMap::new();
        for s in &sites {
            let arity = s["args"].as_u64().unwrap_or(0) as usize;
            let kwargs_keys: Vec<String> = s["kwargs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let arg_consts: Vec<Option<String>> = s["arg_consts"]
                .as_array()
                .map(|a| a.iter().map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let site_uses: Vec<String> = s["uses"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let key = (arity, kwargs_keys, arg_consts);
            unique_sigs.entry(key).or_default().extend(site_uses);
        }
        if unique_sigs.is_empty() {
            unique_sigs.insert((0, Vec::new(), Vec::new()), Vec::new());
        }

        for ((arity, kwargs_keys, arg_consts), site_uses) in unique_sigs {
            let mut kw = HashMap::new();
            for k in &kwargs_keys {
                if k != "?" {
                    kw.insert(k.clone(), PyRep::Integer(1));
                }
            }
            let candidates = build_candidate_arg_sets(&service, &method, arity, &arg_consts);
            let effective_shape = if rpc_key == "slash.SlashCmd" {
                let mut m = serde_json::Map::new();
                for u in &site_uses {
                    if let Some((p, op)) = u.split_once(' ') {
                        m.entry(p.to_string())
                            .or_insert_with(|| Value::Array(Vec::new()))
                            .as_array_mut()
                            .unwrap()
                            .push(Value::String(op.to_string()));
                    }
                }
                m
            } else {
                reply_shape.clone()
            };
            pending_sigs.push(PendingSig {
                rpc_key: rpc_key.clone(),
                service: service.clone(),
                method: method.clone(),
                kw,
                candidates,
                effective_shape,
            });
        }
    }

    let total_unique_rpcs: BTreeSet<String> =
        pending_sigs.iter().map(|p| p.rpc_key.clone()).collect();

    // Pass 1: Run all signatures while docked (station_id = Some(STATION_ID))
    // Pass 2: Run remaining failed signatures while in space (station_id = None)
    for in_space in [false, true] {
        let mut next_pending = Vec::new();
        for sig in pending_sigs {
            let mut sig_ok = false;
            for cand_args in &sig.candidates {
                if in_space {
                    restore_session_in_space(&ctx, &mut sess);
                } else {
                    restore_session_docked(&ctx, &mut sess);
                }
                let bound_id = bind_for_service(&ctx, &sess, &sig.service);
                let res = ctx
                    .dispatch_rpc(
                        &mut sess,
                        &sig.service,
                        bound_id,
                        &sig.method,
                        cand_args,
                        &sig.kw,
                        None,
                    )
                    .await;
                for (ev, nargs) in sess.drain_notifications() {
                    all_emitted_notifs.push((ev, nargs));
                }
                for n in ctx.drain_notifications_for_char(CHAR_ID) {
                    all_emitted_notifs.push((n.event, n.args));
                }

                match res {
                    Ok(rep) => {
                        succeeded_rpcs.insert(sig.rpc_key.clone());
                        sig_ok = true;
                        let mut v =
                            validate_reply_shape(&sig.rpc_key, &rep, &sig.effective_shape);
                        if !v.is_empty() {
                            if let Ok(rep_macho) = ctx
                                .dispatch_rpc(
                                    &mut sess,
                                    &sig.service,
                                    bound_id,
                                    &sig.method,
                                    cand_args,
                                    &sig.kw,
                                    Some("machoNet:26000"),
                                )
                                .await
                            {
                                let v_macho = validate_reply_shape(
                                    &sig.rpc_key,
                                    &rep_macho,
                                    &sig.effective_shape,
                                );
                                if v_macho.len() < v.len() {
                                    v = v_macho;
                                }
                            }
                        }
                        shape_violations.extend(v);
                        break;
                    }
                    Err(e) if e.code == "UnimplementedRPC" => {
                        saw_unimplemented_rpcs.insert(sig.rpc_key.clone());
                    }
                    Err(_) => {}
                }
            }
            if !sig_ok {
                next_pending.push(sig);
            }
        }
        pending_sigs = next_pending;
    }

    let unimplemented_rpcs: BTreeSet<String> = saw_unimplemented_rpcs
        .into_iter()
        .filter(|k| !succeeded_rpcs.contains(k))
        .collect();
    let succeeded_count = succeeded_rpcs.len();
    let domain_rejected_count = total_unique_rpcs
        .len()
        .saturating_sub(succeeded_count + unimplemented_rpcs.len());

    // Validate all emitted notifications against `notify` expectations (both arity and `reads`)
    let notify_map = root["notify"].as_object().expect("notify map");
    let mut notify_violations = Vec::new();
    for (ev, nargs) in &all_emitted_notifs {
        // `OnSessionChanged` in `session.notifications` is an internal server trigger intercepted by
        // `macho_listener.rs` to construct a wire `SessionChangeNotification` packet; on the client,
        // `arg1(sess)` is the client's local `blue.pyos.synchro` session object.
        if ev == "OnSessionChanged" {
            continue;
        }
        let Some(nspec) = notify_map.get(ev) else {
            continue;
        };
        let mut arity_min = nspec["arity_min"].as_u64().unwrap_or(0) as usize;
        let mut arity_max = nspec["arity_max"].as_u64().unwrap_or(usize::MAX as u64) as usize;
        // Recompute exact Python arity bounds from `param_variants` so `*args` handlers allow unlimited max arity
        if let Some(variants) = nspec["param_variants"].as_array() {
            if !variants.is_empty() {
                let mut req_min = 0usize;
                let mut req_max = usize::MAX;
                for v in variants {
                    if let Some(plist) = v.get(0).and_then(|x| x.as_array()) {
                        let pstrs: Vec<&str> =
                            plist.iter().filter_map(|x| x.as_str()).collect();
                        let has_varargs = pstrs
                            .iter()
                            .any(|p| p.starts_with('*') && !p.starts_with("**"));
                        let pos_count = pstrs.iter().filter(|p| !p.starts_with('*')).count();
                        req_min = req_min.max(pos_count);
                        if !has_varargs {
                            req_max = req_max.min(pos_count);
                        }
                    }
                }
                if req_min <= req_max {
                    arity_min = req_min;
                    arity_max = req_max;
                }
            }
        }
        let nargs = eve_server::quasar_grpc::normalize_macho_notification(ev, nargs.clone());
        let nargs = &nargs;
        if nargs.len() < arity_min || nargs.len() > arity_max {
            notify_violations.push(format!(
                "notify:{ev}: emitted {} args, client expects {arity_min}..={arity_max} ({nargs:?})",
                nargs.len()
            ));
            continue;
        }
        // Validate notification argument `reads` (e.g. `arg0(data) attr:foo`, `arg0(times) unpack:2`)
        if let Some(reads) = nspec["reads"].as_array() {
            let mut arg_shapes: HashMap<usize, serde_json::Map<String, Value>> = HashMap::new();
            for r in reads.iter().filter_map(|x| x.as_str()) {
                // `SpaceMgr.OnDamageStateChange(shipID, damageState)` receives a 3-tuple from the server;
                // `HealthWidget.OnDamageStateChange` is a client-internal scatter event with a 5-tuple.
                if ev == "OnDamageStateChange" && r.contains("unpack:5") {
                    continue;
                }
                let Some(rest) = r.strip_prefix("arg") else {
                    continue;
                };
                let Some(open_paren) = rest.find('(') else {
                    continue;
                };
                let Ok(arg_idx) = rest[..open_paren].parse::<usize>() else {
                    continue;
                };
                let Some(close_paren) = rest.find(')') else {
                    continue;
                };
                let param_name = &rest[open_paren + 1..close_paren];
                // Skip if param_name is the `*args` tuple itself
                if nspec["params"]
                    .as_array()
                    .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some(&format!("*{param_name}"))))
                {
                    continue;
                }
                let after_paren = &rest[close_paren + 1..];
                let Some((subpath, op)) = after_paren.split_once(' ') else {
                    continue;
                };
                let path_key = if subpath.is_empty() {
                    "$".to_string()
                } else {
                    subpath.to_string()
                };
                arg_shapes
                    .entry(arg_idx)
                    .or_default()
                    .entry(path_key)
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .unwrap()
                    .push(Value::String(op.to_string()));
            }
            for (arg_idx, shape_map) in arg_shapes {
                if let Some(arg_val) = nargs.get(arg_idx) {
                    let v = validate_reply_shape(
                        &format!("notify:{ev}[arg{arg_idx}]"),
                        arg_val,
                        &shape_map,
                    );
                    notify_violations.extend(v);
                }
            }
        }
    }

    println!(
        "=== RPC CONTRACT REPORT: succeeded={succeeded_count}, domain_rejected={domain_rejected_count}, unimplemented={}, shape_violations={}, notify_violations={} ===",
        unimplemented_rpcs.len(),
        shape_violations.len(),
        notify_violations.len(),
    );
    for u in &unimplemented_rpcs {
        println!("  [UNIMPLEMENTED] {u}");
    }
    for v in &shape_violations {
        println!("  [SHAPE VIOLATION] {v}");
    }
    for nv in &notify_violations {
        println!("  [NOTIFY VIOLATION] {nv}");
    }

    assert!(
        unimplemented_rpcs.is_empty() && shape_violations.is_empty() && notify_violations.is_empty(),
        "Found {} unimplemented RPCs, {} reply_shape violations, and {} notify violations",
        unimplemented_rpcs.len(),
        shape_violations.len(),
        notify_violations.len()
    );
}
