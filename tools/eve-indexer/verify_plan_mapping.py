#!/usr/bin/env python3
"""
tools/eve-indexer/verify_plan_mapping.py

Deterministically generates and verifies the 12-domain Master Mapping Table in `PLAN.md §3.12`
and enforces repository-wide specification integrity across `PLAN.md`, `README.md`,
`docs/AGENT_HANDOFF.md`, `docs/index/PARITY_MATRIX.md`, and `docs/spec/*.md`:

1. §3.12 Bijection: 0 invented, 0 unmapped, 0 duplicate services/domains against
   `docs/index/rpc_service_catalog.json` (216 services: 193 V24.01 active + 23 Crucible legacy-only)
   and `docs/index/quasar_protobuf_catalog.json` (135 domains: 69 eve_public + 66 eve internal).
2. Prose Service & Quasar Token Audit: Scans all markdown files for backticked `*Mgr|*Svc|*Service|
   *Proxy|*Broker|*Manager|*Registry|*Notifier`, `RemoteSvc('...')`, and `eve_public.<domain>` tokens,
   verifying that 100% are in the catalogs or the explicit client-only allowlist.
3. JSON Artifact Count Consistency: Verifies internal mathematical consistency across all `docs/index/*.json`
   catalogs and rejects stale counts in `PLAN.md` and `README.md`.
4. Relative File Link Existence: Verifies that every relative markdown link `[...](./...)` or `[...](../...)`
   across all specification and documentation files resolves to an existing path on disk.
"""

import argparse
import glob
import json
import os
import re
import sys

DOMAIN_ASSIGNMENTS = [
    (
        "1. `auth_cache.rs` *(Core Bus, Auth, Config, Caching, Telemetry & GM)*",
        [
            "alert", "authentication", "bannedwords", "browserLockdownSvc", "bulkMgr",
            "cache", "clientStatLogger", "clientStatsMgr", "config", "debug",
            "devToolsProvider", "eveguard_report", "eventLog", "externalQueueMgr", "i2",
            "infoGatheringMgr", "ingamereport", "kiringMgr", "languageSvc", "loadService",
            "localizationServer", "lookupSvc", "machoNet", "multiLoginBlocker", "objectCaching",
            "onlineStatus", "overviewPresetMgr", "petitioner", "photoUploadSvc", "pingService",
            "populationCap", "publicQaToolsServer", "securityMgr", "serviceGatewayMgr", "sessionMgr",
            "sharedSettingsMgr", "slash", "survey", "tcpRawProxyService", "userSvc",
            "voiceMgr", "worldSpaceServer", "zActionServer",
        ],
        [
            "app", "auth", "botdetection", "cdn", "client", "color", "csat", "debug",
            "device", "entitlement", "eve", "eve_public_root", "graphics", "id", "idempotency",
            "legacy", "legal", "localization", "login", "math", "monolith", "network",
            "pii", "presence", "quasar", "search", "thirdpartyapi", "ui", "user", "vanguard",
        ],
    ),
    (
        "2. `character_skills.rs` *(Character Creation, PaperDoll, Clones, Skills & Progression)*",
        [
            "alphaInjectorMgr", "certificateMgr", "charMgr", "charUnboundMgr", "characterEnergyMgr",
            "expertSystemMgr", "home_station", "jumpCloneSvc", "milestoneMgr", "monolithTaskTrackerMgr",
            "nonDiminishingInjectionMgr", "paperDollServer", "skillMgr2", "subscriptionMgr", "voucher",
        ],
        [
            "academy", "achievement", "ancestry", "bloodline", "career", "character",
            "implant", "omega", "owner", "race", "school", "skill", "starterspace",
        ],
    ),
    (
        "3. `inventory_fitting.rs` *(Inventory, Dogma, Ships, Fitting, Mutaplasmids & Insurance)*",
        [
            "allianceFittingMgr", "charFittingMgr", "corpFittingMgr", "crateService", "dogmaIM",
            "dynamicItemService", "eject", "encodedItems", "insuranceSvc", "invbroker",
            "inventoryAssetHoldingMgr", "itemLocking", "repackagingSvc", "repairSvc", "ship",
            "shipKillCounter", "shipStanceMgr",
        ],
        [
            "asset", "assetholding", "capacitor", "container", "dogma", "effect",
            "inventory", "module", "ship",
        ],
    ),
    (
        "4. `station_structure.rs` *(Stations, Upwell Structures, Tethering, Asset Safety & POS)*",
        [
            "autoMoonMining", "corpStationMgr", "moonExtractions", "posMgr", "station",
            "stationSvc", "structure", "structureAssetSafety", "structureCargoDelivery",
            "structureCompressionMgr", "structureControl", "structureCynoBeaconMgr",
            "structureDeliveries", "structureDeployment", "structureDirectory", "structureDocking",
            "structureGuests", "structureHangarViewMgr", "structureJumpBridgeMgr",
            "structureSettings", "structureVulnerability",
        ],
        [
            "moon", "starbase", "station", "structure",
        ],
    ),
    (
        "5. `space_destiny.rs` *(Destiny Physics, Warp, Jumps, Scanning, Wormholes, Filaments & Abyss)*",
        [
            "abyssalMgr", "abyssal_content_manager", "accessGroupBookmarkMgr", "beyonce",
            "bookmark", "corpBookmarkMgr", "hackingMgr", "jumpTimers", "map",
            "movementServer", "pvpFilamentMgr", "randomJumpMgr", "scanMgr", "voidSpaceMgr",
            "warpVectorMgr", "wormholeMgr",
        ],
        [
            "asteroidbelt", "bookmark", "bubble", "constellation", "deadspace", "probe",
            "region", "solarsystem", "space", "spacecomponent", "star", "stargate",
            "universe", "wormhole",
        ],
    ),
    (
        "6. `combat_crimewatch.rs` *(1 Hz Combat, Drones, Fighters, Doomsdays, CrimeWatch & Bounties)*",
        [
            "aggressionMgr", "bountyProxy", "crimewatch", "entity", "fighterMgr",
            "killRightMgr", "superWeaponMgr", "warStatisticMgr",
        ],
        [
            "ai", "duel", "fighter", "killmail", "npc", "pirate", "piratestronghold",
            "pvp", "securitystatus", "tag",
        ],
    ),
    (
        "7. `market_contracts.rs` *(Markets, Wallet, PLEX, Contracts, HyperNet, Trade & LP Stores)*",
        [
            "FastCheckoutService", "LPStore", "LPStoreMgr", "LPSvc", "account", "billMgr",
            "contextualOfferMgr", "contractProxy", "marketProxy", "nes_intro", "raffleMgr",
            "raffleProxy", "rewardMgr", "storeManager", "trademgr", "vaultManager",
        ],
        [
            "accounting", "contextualoffer", "contract", "fulfillment", "isk",
            "loyaltypoints", "market", "payment", "plex", "raffle", "store", "vendors",
        ],
    ),
    (
        "8. `industry_pi.rs` *(Blueprints, Manufacturing, Invention, Reprocessing, Compression & PI)*",
        [
            "blueprintManager", "characterMiningLedger", "colonyResourcesMgr", "corpMiningLedger",
            "facilityManager", "factory", "inSpaceCompressionMgr", "industryManager",
            "industryMonitor", "miningScanMgr", "planetBaseBroker", "planetMgr",
            "planetOrbitalRegistryBroker", "ramProxy", "reprocessingSvc",
        ],
        [
            "blueprint", "industry", "ore", "planet", "planetinteraction",
        ],
    ),
    (
        "9. `agents_missions.rs` *(Agents, Missions, NPE, Dungeons, Incursions, Homefronts & Epic Arcs)*",
        [
            "RWManager", "activityMgr", "agentMgr", "air_npe", "custom_agency_provider",
            "dungeon", "dungeonExplorationMgr", "dungeonInstanceCacheMgr", "encounterSpawnServer",
            "epicArcStatus", "keeper", "missionMgr", "missionTrackerMgr", "node_graph",
            "operationsManager", "scenario", "taleMgr", "tutorialSvc",
        ],
        [
            "agent", "content", "dailygoal", "dungeon", "epicarc", "goal", "mission",
            "narrative", "nodegraph", "operation", "opportunity", "tale",
        ],
    ),
    (
        "10. `social_corp_fleet.rs` *(Chat, Mail, Fleets, Corporations, Alliances, Standings & Wars)*",
        [
            "LSC", "XmppChatMgr", "allianceRegistry", "calendarMgr", "calendarProxy",
            "chatAuthenticationService", "corpRecProxy", "corpRegistry", "corpmgr",
            "corporationSvc", "fleetMgr", "fleetObjectHandler", "fleetProxy", "mailMgr",
            "mailingListsMgr", "mutualWarInviteMgr", "notificationMgr", "peaceTreatyMgr",
            "search", "standingMgr", "voteManager", "warRegistry", "warsInfoMgr",
        ],
        [
            "acl", "alliance", "calendar", "chat", "contact", "corporation",
            "corporationgoal", "fleet", "freelance", "mail", "medal", "membership",
            "notification", "standing", "war",
        ],
    ),
    (
        "11. `sov_fw_insurgency.rs` *(Sovereignty, Equinox, Faction Warfare, Insurgencies & Dynamic Bounties)*",
        [
            "devIndexManager", "dynamicBountyMgr", "dynamicResourceCacheMgr", "essMgr",
            "facWarMgr", "fwCharacterEnlistmentMgr", "fwVictoryPointMgr", "fwWarzoneSolarsystem",
            "insurgencySolarsystem", "shipCasterLandingPadMgr", "shipCasterLauncherMgr",
            "shipcasterTravelMgr", "solarsystemInterferenceMgr", "sovMgr", "starterShipcasterTravelMgr",
        ],
        [
            "dynamicbountysystem", "faction", "factionwarfare", "sovereignty",
        ],
    ),
    (
        "12. `cosmetics_events.rs` *(SKINR, Paragon Hub, Project Discovery, Tournaments & Seasonal Events)*",
        [
            "ProjectDiscovery", "communityTrackMgr", "holoscreenMgr", "loginCampaignManager",
            "loginRewardFacilities", "seasonManager", "seasonalLoginCampaignManager",
            "shipCosmeticsMgr", "tourneyMgr",
        ],
        [
            "battlepass", "campaign", "communitytrack", "cosmetic", "projectdiscovery",
            "seasonalevent", "tournament",
        ],
    ),
]

from index_server_logic import CRUCIBLE_CLASS_TO_SERVICE

# Client-only Python services in V24.01 `code.ccp` (`sm.GetService(...)`), framework helpers, and Crucible C++ class names
CLIENT_ONLY_SERVICES_ALLOWLIST = {
    "skillQueueSvc",
    "invCache",
    "marketQuote",
    "godma",
    "michelle",
    "agents",
    "epicArc",
    "noddiTestRunner",
    "node_graph",
    "publicGatewaySvc",
    "fakePublicGatewayService",
    "ServiceManager",
    "_CloneGradeSvc",
    "RemoteSvc",
    "ProxySvc",
    "ConnectToRemoteService",
    "SdeRegistry",
    "SystemRegistry",
    "ComponentRegistry",
} | set(CRUCIBLE_CLASS_TO_SERVICE.keys())


def render_markdown_table(total_rpc, v24_active, legacy_only, total_quasar):
    lines = [
        "| Domain Module (`crates/eve-server/src/services/`) | Assigned `machoNet` Remote Services & Monikers (`%d` Cataloged: `%d` V24.01 Active + `%d` Crucible Legacy-Only) | Assigned Quasar Protobuf Domains (`%d` Total: `69` `eve_public.*` + `66` `eve.*` Internal) |"
        % (total_rpc, v24_active, legacy_only, total_quasar),
        "|---|---|---|",
    ]
    for domain_label, svcs, q_domains in DOMAIN_ASSIGNMENTS:
        svcs_sorted = sorted(svcs)
        q_sorted = sorted(q_domains)
        svc_cell = ", ".join("`%s`" % s for s in svcs_sorted) + (" (`%d services`)" % len(svcs_sorted))
        q_cell = ", ".join("`%s`" % q for q in q_sorted)
        lines.append("| **%s** | %s | %s |" % (domain_label, svc_cell, q_cell))
    return "\n".join(lines)


def parse_plan_section_3_12(plan_text):
    if "### 3.12" not in plan_text:
        raise ValueError("Could not find '### 3.12' in PLAN.md")
    sec = plan_text.split("### 3.12", 1)[1].split("\n## 4.", 1)[0]
    rows = [ln for ln in sec.splitlines() if ln.startswith("| **")]
    rpc_tokens = []
    quasar_tokens = []
    count_mismatches = []

    for row in rows:
        cols = [c.strip() for c in row.split("|")[1:-1]]
        if len(cols) < 3:
            continue
        mod_name, rpc_col, quasar_col = cols[0], cols[1], cols[2]
        r_toks = [
            t.strip()
            for t in re.findall(r"`([^`]+)`", rpc_col)
            if not t.strip().endswith("services")
        ]
        q_toks = [t.strip() for t in re.findall(r"`([^`]+)`", quasar_col)]
        rpc_tokens.extend(r_toks)
        quasar_tokens.extend(q_toks)

        m = re.search(r"\(`(\d+)\s+services`\)", rpc_col)
        if m and int(m.group(1)) != len(r_toks):
            count_mismatches.append((mod_name, int(m.group(1)), len(r_toks)))

    return rows, rpc_tokens, quasar_tokens, count_mismatches


def verify_prose_and_links(root, rpc_set, quasar_set):
    md_files = [
        os.path.join(root, "PLAN.md"),
        os.path.join(root, "README.md"),
        os.path.join(root, "docs", "AGENT_HANDOFF.md"),
        os.path.join(root, "docs", "index", "PARITY_MATRIX.md"),
    ] + sorted(glob.glob(os.path.join(root, "docs", "spec", "*.md")))

    bad_services = []
    bad_quasar = []
    broken_links = []
    stale_counts = []

    link_re = re.compile(r"\[[^\]]+\]\((\.\.?/[^)#\s]+)")

    for fpath in md_files:
        rel_f = os.path.relpath(fpath, root)
        with open(fpath, "r", encoding="utf-8") as f:
            text = f.read()

        # Check for stale Pass 1 counts
        for stale_pat in ("217 services", "217 Services", "217-Service", "217 machoNet", "24 Crucible", "1,390", "822` effect"):
            if stale_pat in text:
                stale_counts.append((rel_f, stale_pat))

        # Scan prose outside §3.12
        prose = text
        if rel_f == "PLAN.md" and "### 3.12" in text and "\n## 4." in text:
            prose = text.split("### 3.12", 1)[0] + text.split("\n## 4.", 1)[1]

        cands = set(re.findall(r"`([A-Za-z_][A-Za-z0-9_]*(?:Mgr|Svc|Service|Proxy|Broker|Manager|Registry|Notifier))`", prose))
        cands |= set(re.findall(r"RemoteSvc\(['\"]([A-Za-z_0-9]+)['\"]\)", prose))
        for c in sorted(cands):
            if c not in rpc_set and c not in CLIENT_ONLY_SERVICES_ALLOWLIST:
                bad_services.append((rel_f, c))

        q_cands = set(re.findall(r"eve_public\.([a-z_]+)", prose))
        for qc in sorted(q_cands):
            if qc not in quasar_set:
                bad_quasar.append((rel_f, qc))

        # Verify relative links
        fdir = os.path.dirname(fpath)
        for m in link_re.finditer(text):
            rel_target = m.group(1)
            abs_target = os.path.normpath(os.path.join(fdir, rel_target))
            if not os.path.exists(abs_target):
                broken_links.append((rel_f, rel_target, abs_target))

    return bad_services, bad_quasar, broken_links, stale_counts


def verify_json_artifacts(root, rpc_cat, quasar_cat):
    errors = []
    cc = json.load(open(os.path.join(root, "docs", "index", "crucible_comparison.json"), encoding="utf-8"))
    do = json.load(open(os.path.join(root, "docs", "index", "destiny_opcodes.json"), encoding="utf-8"))
    de = json.load(open(os.path.join(root, "docs", "index", "dogma_effects.json"), encoding="utf-8"))

    # 1. Verify zero un-evidenced services in rpc_service_catalog.json
    unevidenced = [
        k for k, v in rpc_cat.items()
        if not v.get("v24_client_call_files") and not v.get("evemu_crucible_classes")
    ]
    if unevidenced:
        errors.append("Un-evidenced services in rpc_service_catalog.json: %s" % unevidenced)

    # 2. Verify crucible_comparison.json partition sums to total_catalog_services (216)
    s = cc.get("summary", {})
    part_sum = s.get("both_v24_and_crucible", 0) + s.get("v24_only_new_services", 0) + s.get("crucible_legacy_only_services", 0)
    if part_sum != len(rpc_cat) or s.get("total_catalog_services") != len(rpc_cat):
        errors.append(
            "crucible_comparison.json partition mismatch: %d + %d + %d = %d vs len(rpc_cat)=%d"
            % (
                s.get("both_v24_and_crucible", 0),
                s.get("v24_only_new_services", 0),
                s.get("crucible_legacy_only_services", 0),
                part_sum,
                len(rpc_cat),
            )
        )

    # 3. Verify Quasar eve_public (69) + eve internal (66) == 135
    if s.get("quasar_eve_public_domains_count", 0) + s.get("quasar_eve_internal_domains_count", 0) != len(quasar_cat):
        errors.append("Quasar eve_public + eve_internal count mismatch in crucible_comparison.json")

    # 4. Verify destiny_opcodes.json has 13 ball_modes and >30 v24_destiny_actions
    if len(do.get("ball_modes", {})) != 13:
        errors.append("Expected 13 DSTBALL_* ball_modes in destiny_opcodes.json, got %d" % len(do.get("ball_modes", {})))
    if len(do.get("v24_destiny_actions", [])) < 30:
        errors.append("Expected >=30 v24_destiny_actions in destiny_opcodes.json, got %d" % len(do.get("v24_destiny_actions", [])))

    # 5. Verify dogma_effects.json constants
    vc = de.get("v24_dogma_constants", {})
    if vc.get("attribute_constants_count") != 1343 or vc.get("effect_constants_count") != 305:
        errors.append("Unexpected dogma/const.pyj counts in dogma_effects.json: %s" % vc)

    return errors


def main():
    parser = argparse.ArgumentParser(description="Verify PLAN.md §3.12, prose tokens, JSON counts, and relative links")
    parser.add_argument("--evomu-root", default=os.path.abspath(os.path.join(os.path.dirname(__file__), "../..")))
    parser.add_argument("--check", action="store_true", help="Exit non-zero if any discrepancy is found")
    parser.add_argument("--print-table", action="store_true", help="Print the canonical Markdown table")
    args = parser.parse_args()

    rpc_path = os.path.join(args.evomu_root, "docs", "index", "rpc_service_catalog.json")
    quasar_path = os.path.join(args.evomu_root, "docs", "index", "quasar_protobuf_catalog.json")
    plan_path = os.path.join(args.evomu_root, "PLAN.md")

    with open(rpc_path, "r", encoding="utf-8") as f:
        rpc_cat = json.load(f)
    with open(quasar_path, "r", encoding="utf-8") as f:
        quasar_cat = json.load(f)

    rpc_set = set(rpc_cat.keys())
    quasar_set = set(quasar_cat.keys())
    v24_active = sum(1 for v in rpc_cat.values() if v.get("v24_client_call_files"))
    legacy_only = len(rpc_cat) - v24_active

    if args.print_table:
        print(render_markdown_table(len(rpc_cat), v24_active, legacy_only, len(quasar_cat)))
        return

    with open(plan_path, "r", encoding="utf-8") as f:
        plan_text = f.read()

    rows, rpc_tokens, quasar_tokens, count_mismatches = parse_plan_section_3_12(plan_text)
    rpc_table_set = set(rpc_tokens)
    quasar_table_set = set(quasar_tokens)

    rpc_invented = sorted(rpc_table_set - rpc_set)
    rpc_unmapped = sorted(rpc_set - rpc_table_set)
    rpc_dups = sorted(set(x for x in rpc_tokens if rpc_tokens.count(x) > 1))

    q_invented = sorted(quasar_table_set - quasar_set)
    q_unmapped = sorted(quasar_set - quasar_table_set)
    q_dups = sorted(set(x for x in quasar_tokens if quasar_tokens.count(x) > 1))

    bad_services, bad_quasar, broken_links, stale_counts = verify_prose_and_links(args.evomu_root, rpc_set, quasar_set)
    json_errors = verify_json_artifacts(args.evomu_root, rpc_cat, quasar_cat)

    print("=== PLAN.md §3.12 & Repository Specification Audit ===")
    print("Rows parsed: %d" % len(rows))
    print(
        "RPC Services:    table=%d (unique=%d), catalog=%d (%d V24.01 active + %d Crucible legacy-only)"
        % (len(rpc_tokens), len(rpc_table_set), len(rpc_set), v24_active, legacy_only)
    )
    print("  Invented (in table, not catalog): %d %s" % (len(rpc_invented), rpc_invented))
    print("  Unmapped (in catalog, not table): %d %s" % (len(rpc_unmapped), rpc_unmapped))
    print("  Duplicates in table:              %d %s" % (len(rpc_dups), rpc_dups))
    print("  Row count label mismatches:       %d %s" % (len(count_mismatches), count_mismatches))
    print(
        "Quasar Domains:  table=%d (unique=%d), catalog=%d"
        % (len(quasar_tokens), len(quasar_table_set), len(quasar_set))
    )
    print("  Invented (in table, not catalog): %d %s" % (len(q_invented), q_invented))
    print("  Unmapped (in catalog, not table): %d %s" % (len(q_unmapped), q_unmapped))
    print("  Duplicates in table:              %d %s" % (len(q_dups), q_dups))
    print("Prose & Link Audit across PLAN.md, README.md, AGENT_HANDOFF.md, PARITY_MATRIX.md, docs/spec/*.md:")
    print("  Uncataloged service tokens in prose: %d %s" % (len(bad_services), bad_services))
    print("  Uncataloged Quasar tokens in prose:  %d %s" % (len(bad_quasar), bad_quasar))
    print("  Broken relative file links:          %d %s" % (len(broken_links), broken_links))
    print("  Stale Pass-1 count strings:          %d %s" % (len(stale_counts), stale_counts))
    print("  JSON artifact consistency errors:    %d %s" % (len(json_errors), json_errors))

    ok = not (
        rpc_invented
        or rpc_unmapped
        or rpc_dups
        or count_mismatches
        or q_invented
        or q_unmapped
        or q_dups
        or bad_services
        or bad_quasar
        or broken_links
        or stale_counts
        or json_errors
    )
    if ok:
        print("[PASS] 100% bijection, prose token, JSON count, and link integrity verified across all files.")
    else:
        print("[FAIL] Discrepancies detected.")
        if args.check:
            sys.exit(1)


if __name__ == "__main__":
    main()
