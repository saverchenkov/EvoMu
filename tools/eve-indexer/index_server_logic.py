#!/usr/bin/env python3
"""
tools/eve-indexer/index_server_logic.py — Exhaustive EVE Online Server Logic Indexer.

Extracts and cross-references:
1. `evemu_Crucible` C++ service registrations (`this->Add("Method", &Class::Method)`), XML packet definitions
   (`Destiny.xmlp`), Dogma tables (`dgmEffects.sql.gz`, `dgmOperands.sql.gz`), and implementation completeness.
2. Live EVE Online V24.01 (`build 3619879`) `code.ccp` Python 2.7 bytecode via two-pass opcode disassembly
   (`RemoteSvc`, `ProxySvc`, `ConnectToRemoteService`, `Moniker`, `eveMoniker.Get*`, `__notifyevents__`,
   and `eveProto/generated/` Quasar Protobuf domain schemas).
3. Live EVE Online V24.01 `resfileindex.txt` static data resources (`res:/staticdata/*.fsdbinary` and `*.static`).
4. Local `~/ai/eve-wiki` (`catalog.json` & `articles/*.md`) mathematical formulas and PvE/mechanics articles.
5. Current `EvoMu` Rust `crates/eve-server/src/` implemented `(service, method)` handlers.

Outputs 9 machine-readable & human-readable catalogs under `docs/index/`:
- `rpc_service_catalog.json`
- `crucible_comparison.json`
- `client_notifications.json`
- `quasar_protobuf_catalog.json`
- `staticdata_catalog.json`
- `dogma_effects.json`
- `destiny_opcodes.json`
- `wiki_mechanics_catalog.json`
- `PARITY_MATRIX.md`
"""

import argparse
import collections
import gzip
import json
import os
import re
import struct
import zipfile
import zlib


# Complete normalization table mapping every evemu_Crucible C++ service/bound class to its canonical V24.01 service name
CRUCIBLE_CLASS_TO_SERVICE = {
    "AccountService": "account",
    "AgentMgrService": "agentMgr",
    "AgentBound": "agentMgr",
    "AggressionMgrService": "aggressionMgr",
    "AggressionMgrBound": "aggressionMgr",
    "AlertService": "alert",
    "AllianceRegistryService": "allianceRegistry",
    "AllianceRegistry": "allianceRegistry",
    "AllianceBound": "allianceRegistry",
    "AuthenticationService": "authentication",
    "AuthService": "authentication",
    "BeyonceService": "beyonce",
    "BeyonceBound": "beyonce",
    "BillMgrService": "billMgr",
    "BillMgr": "billMgr",
    "BookmarkService": "bookmark",
    "BrowserLockdownService": "browserLockdownSvc",
    "BulkMgrService": "bulkMgr",
    "CalendarMgrService": "calendarMgr",
    "CalendarProxy": "calendarProxy",
    "CertificateMgrService": "certificateMgr",
    "CharFittingMgrService": "charFittingMgr",
    "CharFittingMgr": "charFittingMgr",
    "CharMgrService": "charMgr",
    "CharMgrBound": "charMgr",
    "CharUnboundMgrService": "charUnboundMgr",
    "ClientStatsMgr": "clientStatsMgr",
    "ClientStatLogger": "clientStatLogger",
    "ConfigService": "config",
    "ContractMgrService": "contractMgr",
    "ContractProxy": "contractProxy",
    "CorpBookmarkMgr": "corpBookmarkMgr",
    "CorpFittingMgr": "corpFittingMgr",
    "CorpmgrService": "corpmgr",
    "CorpMgrService": "corpmgr",
    "CorporationService": "corporationSvc",
    "CorpRegistryService": "corpRegistry",
    "CorpRegistryBound": "corpRegistry",
    "CorpStationMgrService": "corpStationMgr",
    "CorpStationMgr": "corpStationMgr",
    "CorpStationMgrBound": "corpStationMgr",
    "CorpStationMgrIMBound": "corpStationMgr",
    "CrimeWatchService": "crimewatch",
    "DevToolsProviderService": "devToolsProvider",
    "DogmaService": "dogmaIM",
    "DogmaIMService": "dogmaIM",
    "DogmaIMBound": "dogmaIM",
    "DungeonExplorationMgrService": "dungeonExplorationMgr",
    "DungeonService": "dungeon",
    "EntityBound": "entity",
    "EpicArcService": "epicArcStatus",
    "FactoryService": "factory",
    "FacWarMgrService": "facWarMgr",
    "FactionWarMgrService": "facWarMgr",
    "FleetManager": "fleetMgr",
    "FleetObject": "fleetObjectHandler",
    "FleetBound": "fleetObjectHandler",
    "FleetProxy": "fleetProxy",
    "HoloscreenMgrService": "holoscreenMgr",
    "IndexManager": "devIndexManager",
    "InfoGatheringMgr": "infoGatheringMgr",
    "InsuranceService": "insuranceSvc",
    "InsuranceBound": "insuranceSvc",
    "InvBrokerService": "invbroker",
    "InvBrokerBound": "invbroker",
    "JumpCloneService": "jumpCloneSvc",
    "JumpCloneBound": "jumpCloneSvc",
    "KeeperService": "keeper",
    "KeeperBound": "keeper",
    "LanguageService": "languageSvc",
    "LocalizationServerService": "localizationServer",
    "LookupService": "lookupSvc",
    "LPCService": "LPStore",
    "LPService": "LPStore",
    "LSCService": "LSC",
    "MailMgrService": "mailMgr",
    "MailingListMgrService": "mailingListsMgr",
    "MapService": "map",
    "MarketProxyService": "marketProxy",
    "MechMgrService": "mechMgr",
    "MiasmaService": "miasma",
    "MissionMgrService": "missionMgr",
    "MovementService": "movementServer",
    "NetService": "machoNet",
    "NotificationMgrService": "notificationMgr",
    "ObjectCachingService": "objectCaching",
    "ObjCacheService": "objectCaching",
    "OnlineStatusService": "onlineStatus",
    "PaperDollService": "paperDollServer",
    "PetitionService": "petitioner",
    "PetitionerService": "petitioner",
    "PhotoUploadService": "photoUploadSvc",
    "PlanetMgrService": "planetMgr",
    "PlanetMgrBound": "planetMgr",
    "PosMgrService": "posMgr",
    "PosMgr": "posMgr",
    "PosMgrBound": "posMgr",
    "RamProxyService": "ramProxy",
    "RepairService": "repairSvc",
    "RepairServiceBound": "repairSvc",
    "ReprocessingService": "reprocessingSvc",
    "ReprocessingServiceBound": "reprocessingSvc",
    "ScanMgrService": "scanMgr",
    "ScanBound": "scanMgr",
    "ScenarioService": "scenario",
    "SearchService": "search",
    "Search": "search",
    "ShipService": "ship",
    "ShipBound": "ship",
    "SkillMgrService": "skillMgr2",
    "SkillMgrBound": "skillMgr2",
    "SlashService": "slash",
    "SovereigntyMgrService": "sovMgr",
    "Standing2Service": "standingMgr",
    "Standing": "standingMgr",
    "StationService": "station",
    "StationSvcService": "stationSvc",
    "StationSvc": "stationSvc",
    "TradeService": "trademgr",
    "TutorialService": "tutorialSvc",
    "UserService": "userSvc",
    "VoiceMgrService": "voiceMgr",
    "VoucherService": "voucher",
    "VoucherBound": "voucher",
    "WarRegistryService": "warRegistry",
    "WarRegistryBound": "warRegistry",
    "WorldSpaceServer": "worldSpaceServer",
    "WormholeMgrService": "wormholeMgr",
    "WormHoleSvc": "wormholeMgr",
}

# Helper functions in `eve/common/script/net/eveMoniker.pyj` that bind remote monikers
MONIKER_HELPERS = {
    "CharGetDogmaLocation": "dogmaIM",
    "GetStationDogmaLocation": "dogmaIM",
    "GetShipAccess": "ship",
    "GetStationShipAccess": "ship",
    "GetEntityAccess": "entity",
    "GetEntityLocation": "entity",
    "GetPOSMgr": "posMgr",
    "GetReprocessingManager": "reprocessingSvc",
    "GetReprocessingManagerEx": "reprocessingSvc",
    "GetCorpStationManager": "corpStationMgr",
    "GetCorpStationManagerEx": "corpStationMgr",
    "GetSolarSystemInventoryMgr": "invbroker",
    "GetStationInventoryMgr": "invbroker",
    "GetInventoryMgr": "invbroker",
    "CharGetCrimewatchLocation": "crimewatch",
    "GetBallPark": "beyonce",
    "GetCourierMissionCreator": "missionMgr",
    "GetAgent": "agentMgr",
    "GetCorpRegistry": "corpRegistry",
    "GetCorpRegistryEx": "corpRegistry",
    "GetAlliance": "allianceRegistry",
    "GetAllianceEx": "allianceRegistry",
    "GetWar": "warRegistry",
    "GetWarEx": "warRegistry",
    "GetWarStatistic": "warStatisticMgr",
    "GetFleet": "fleetObjectHandler",
    "GetPlanet": "planetMgr",
    "GetPlanetBaseManager": "planetBaseBroker",
    "GetPlanetOrbitalRegistry": "planetOrbitalRegistryBroker",
    "GetRepairManager": "repairSvc",
}

# Python 2.7 bytecode opcodes
HAVE_ARGUMENT = 90
RETURN_VALUE = 83
STORE_ATTR = 95
LOAD_CONST = 100
LOAD_NAME = 101
LOAD_ATTR = 106
LOAD_GLOBAL = 116
LOAD_FAST = 124
STORE_FAST = 125
CALL_OPS = (131, 140, 141, 142)


class Py27MarshalReader(object):
    """Pure-Python reader for Python 2.7 `marshal` code objects inside `.pyj` files (preserves raw bytecode bytes)."""

    def __init__(self, data):
        self.data = data
        self.pos = 0
        self.strings = []

    def read_byte(self):
        b = self.data[self.pos]
        self.pos += 1
        return b

    def read_int(self):
        val = struct.unpack_from("<i", self.data, self.pos)[0]
        self.pos += 4
        return val

    def read_int64(self):
        val = struct.unpack_from("<q", self.data, self.pos)[0]
        self.pos += 8
        return val

    def read_bytes(self, n):
        s = self.data[self.pos : self.pos + n]
        self.pos += n
        return s

    def load(self, as_bytes=False):
        t = chr(self.read_byte())
        if t in ("0", "N", "F", "T", ".", "S"):
            return None
        if t == "i":
            return self.read_int()
        if t == "I":
            return self.read_int64()
        if t == "l":
            n = self.read_int()
            self.pos += abs(n) * 2
            return 0
        if t == "f":
            n = self.read_byte()
            self.pos += n
            return 0.0
        if t == "g":
            self.pos += 8
            return 0.0
        if t in ("s", "t", "u"):
            n = self.read_int()
            b = self.read_bytes(n)
            if t == "t":
                self.strings.append(b)
            return b if as_bytes else b.decode("utf-8", errors="replace")
        if t == "R":
            idx = self.read_int()
            b = self.strings[idx] if idx < len(self.strings) else b""
            return b if as_bytes else b.decode("utf-8", errors="replace")
        if t in ("(", "[", "{", "<", ">"):
            if t == "{":
                out = {}
                while True:
                    k = self.load()
                    if k is None and chr(self.data[self.pos - 1]) == "0":
                        break
                    v = self.load()
                    out[str(k)] = v
                return out
            n = self.read_int()
            return [self.load() for _ in range(n)]
        if t == "c":
            self.pos += 16  # argcount, nlocals, stacksize, flags
            code_bytes = self.load(as_bytes=True)
            consts = self.load()
            names = self.load()
            varnames = self.load()
            freevars = self.load()
            cellvars = self.load()
            filename = self.load()
            name = self.load()
            firstlineno = self.read_int()
            lnotab = self.load(as_bytes=True)
            return {
                "__code__": True,
                "code": code_bytes,
                "consts": consts,
                "names": names,
                "varnames": varnames,
                "filename": filename,
                "name": name,
                "lineno": firstlineno,
            }
        raise ValueError("Unknown marshal type %r at %d" % (t, self.pos - 1))


def disasm_py27(code_bytes):
    """Disassembles raw Python 2.7 bytecode into a list of `(opcode, arg)` tuples."""
    if isinstance(code_bytes, str):
        code_bytes = code_bytes.encode("latin1")
    i = 0
    n = len(code_bytes)
    insns = []
    while i < n:
        op = code_bytes[i]
        i += 1
        arg = None
        if op >= HAVE_ARGUMENT and i + 1 < n:
            arg = code_bytes[i] | (code_bytes[i + 1] << 8)
            i += 2
        insns.append((op, arg))
    return insns


def scan_crucible_services(crucible_root):
    """Scans `evemu_Crucible/src/eve-server` for C++ RPC registrations and implementation bodies."""
    src_dir = os.path.join(crucible_root, "src", "eve-server")
    add_re = re.compile(r'this->Add\(\s*"([^"]+)"\s*,\s*&([A-Za-z0-9_]+)::([A-Za-z0-9_]+)\s*\)')
    impl_re_template = r"PyResult\s+%s::%s\s*\([^)]*\)\s*\{([\s\S]*?)\n\}"

    services = collections.defaultdict(lambda: {"cpp_classes": set(), "methods": {}})
    if not os.path.isdir(src_dir):
        return services

    for root, _, files in os.walk(src_dir):
        for fname in sorted(files):
            if not fname.endswith((".cpp", ".h")):
                continue
            fpath = os.path.join(root, fname)
            rel_path = os.path.relpath(fpath, crucible_root)
            with open(fpath, "r", errors="replace") as f:
                content = f.read()

            for match in add_re.finditer(content):
                rpc_name, cls_name, handler_name = match.groups()
                svc_name = CRUCIBLE_CLASS_TO_SERVICE.get(cls_name, cls_name)
                services[svc_name]["cpp_classes"].add(cls_name)

                body_match = re.search(impl_re_template % (re.escape(cls_name), re.escape(handler_name)), content)
                status = "Implemented"
                line_count = 0
                if body_match:
                    body = body_match.group(1)
                    non_empty = [
                        ln.strip()
                        for ln in body.splitlines()
                        if ln.strip() and not ln.strip().startswith("//")
                    ]
                    line_count = len(non_empty)
                    if (
                        "TODO" in body
                        or "stub" in body.lower()
                        or "not implemented" in body.lower()
                        or line_count <= 3
                    ):
                        status = "Stubbed"
                    elif line_count <= 8:
                        status = "Partial"
                else:
                    status = "Unknown"

                services[svc_name]["methods"][rpc_name] = {
                    "cpp_class": cls_name,
                    "handler": handler_name,
                    "file": rel_path,
                    "status": status,
                    "body_lines": line_count,
                }

    return services


def scan_crucible_dogma_and_destiny(crucible_root, code_ccp_path):
    """Extracts `dogma_effects.json` and `destiny_opcodes.json` from `evemu_Crucible` and `code.ccp`."""
    dogma_effects = {
        "effects_count": 0,
        "effects_source": "evemu_Crucible sql/base/dgmEffects.sql.gz (V24.01 uses res:/staticdata/dogmaeffects.fsdbinary)",
        "operands_count": 0,
        "operands": [],
        "sample_effects": [],
        "v24_dogma_constants": {},
    }
    destiny_opcodes = {"xmlp_elements": [], "ball_modes": {}, "v24_destiny_actions": []}

    if crucible_root and os.path.isdir(crucible_root):
        ops_path = os.path.join(crucible_root, "sql", "base", "dgmOperands.sql.gz")
        if os.path.isfile(ops_path):
            data = gzip.open(ops_path, "rt", errors="replace").read()
            for m in re.finditer(r"\((\d+),\x27([^\x27]+)\x27,\x27([^\x27]*)\x27", data):
                dogma_effects["operands"].append({
                    "operandID": int(m.group(1)),
                    "operandKey": m.group(2),
                    "description": m.group(3),
                })
            dogma_effects["operands_count"] = len(dogma_effects["operands"])

        eff_path = os.path.join(crucible_root, "sql", "base", "dgmEffects.sql.gz")
        if os.path.isfile(eff_path):
            data = gzip.open(eff_path, "rt", errors="replace").read()
            all_effs = re.findall(r"\((\d+),\x27([^\x27]+)\x27,(\d+)", data)
            dogma_effects["effects_count"] = len(all_effs)
            dogma_effects["sample_effects"] = [
                {"effectID": int(eid), "effectName": ename, "effectCategory": int(ecat)}
                for eid, ename, ecat in all_effs[:250]
            ]

        xmlp_path = os.path.join(crucible_root, "src", "eve-common", "packets", "Destiny.xmlp")
        if os.path.isfile(xmlp_path):
            with open(xmlp_path, "r", errors="replace") as f:
                destiny_opcodes["xmlp_elements"] = re.findall(r'<elementDef name="([^"]+)"', f.read())

        ds_path = os.path.join(crucible_root, "src", "eve-common", "destiny", "DestinyStructs.h")
        if os.path.isfile(ds_path):
            with open(ds_path, "r", errors="replace") as f:
                ds_text = f.read()
                mode_block = re.search(r"namespace\s+Mode\s*\{\s*enum\s*\{([^}]+)\}", ds_text, re.DOTALL)
                if mode_block:
                    for m in re.finditer(r"([A-Z0-9_]+)\s*=\s*(\d+)", mode_block.group(1)):
                        key = m.group(1)
                        if not key.startswith("DSTBALL_"):
                            key = "DSTBALL_" + key
                        destiny_opcodes["ball_modes"][key] = int(m.group(2))
                for m in re.finditer(r"(DSTBALL_[A-Z0-9_]+)\s*=\s*(\d+)", ds_text):
                    destiny_opcodes["ball_modes"][m.group(1)] = int(m.group(2))

    if code_ccp_path and os.path.isfile(code_ccp_path):
        with zipfile.ZipFile(code_ccp_path, "r") as zf:
            if "dogma/const.pyj" in zf.namelist():
                raw = zlib.decompress(zf.read("dogma/const.pyj"))[8:]
                co = Py27MarshalReader(raw).load()
                names = co.get("names") or []
                consts = co.get("consts") or []
                insns = disasm_py27(co.get("code") or b"")
                op_values = {}
                for idx, (op, arg) in enumerate(insns):
                    if op == LOAD_CONST and (idx + 1) < len(insns) and insns[idx + 1][0] == 90:
                        nm = names[insns[idx + 1][1]]
                        if nm.startswith("dgmAss") or nm.startswith("dgmEff"):
                            op_values[nm] = consts[arg]
                dogma_effects["v24_dogma_constants"] = {
                    "attribute_constants_count": sum(1 for n in names if n.startswith("attribute")),
                    "effect_constants_count": sum(1 for n in names if n.startswith("effect")),
                    "dgm_operator_constants": sorted([n for n in names if n.startswith("dgmAss") or n.startswith("dgmEff")]),
                    "dgm_operator_values": op_values,
                }
            known_destiny_actions = {
                "AddBall", "AddBalls", "AddBalls2", "RemoveBalls", "RemoveBall", "SetState",
                "GotoDirection", "GotoPoint", "Orbit", "FollowBall", "WarpTo",
                "Stop", "SetSpeedFraction", "SetBallInteractive", "SetBallFree",
                "SetBallRadius", "SetBallMass", "SetBallCloak", "CloakBall", "SetBallGlobal",
                "SetBallTroll", "SetBallMassive", "SetBallAgility", "SetBallVelocity",
                "SetBallHarmonic", "SetControllerVariableOnBall", "PackagedAction",
                "ScatterEwars", "OnSpecialFX", "TerminalPlayDestructionEffect",
                "OnDamageStateChange", "OnFleetDamageStateChange", "OnFleetStateChange",
                "OnDroneStateChange", "OnDroneStateChange2", "OnDroneActivityChange",
                "OnDroneControlLost", "OnShipStateUpdate", "OnSlimItemChange",
                "OnSovereigntyChanged", "OnDbuffUpdated", "OnDotVictimUpdated",
                "OnClientControllerEvent", "OnAudioActivated", "OnActivatingWarp",
                "OnDeactivatingWarp", "OnDockingAccepted", "JumpOut",
            }
            found_actions = set()
            for target_pyj in ("eve/client/script/remote/michelle.pyj", "eve/client/script/parklife/spaceMgr.pyj"):
                if target_pyj in zf.namelist():
                    raw = zlib.decompress(zf.read(target_pyj))[8:]
                    co = Py27MarshalReader(raw).load()
                    def walk_c(c):
                        if isinstance(c, dict) and c.get("__code__"):
                            for n in (c.get("names") or []):
                                if n in known_destiny_actions:
                                    found_actions.add(n)
                            for x in (c.get("consts") or []):
                                if isinstance(x, str) and x in known_destiny_actions:
                                    found_actions.add(x)
                                elif isinstance(x, dict):
                                    walk_c(x)
                    walk_c(co)
            destiny_opcodes["v24_destiny_actions"] = sorted(found_actions)

    return dogma_effects, destiny_opcodes


def scan_v24_code_ccp(code_ccp_path):
    """Scans V24.01 `code.ccp` using two-pass Python 2.7 bytecode disassembly for exact RemoteSvc/Moniker calls, `__notifyevents__`, and Quasar Protobufs."""
    remote_services = collections.defaultdict(lambda: {"files": set(), "candidate_methods": set()})
    notify_events = collections.defaultdict(set)
    protobuf_domains = collections.defaultdict(list)

    if not os.path.isfile(code_ccp_path):
        return remote_services, notify_events, protobuf_domains

    def collect_notify_and_pass1(co, pyj_name, class_attr_svc):
        if not isinstance(co, dict) or not co.get("__code__"):
            return
        names = co.get("names") or []
        consts = co.get("consts") or []
        insns = disasm_py27(co.get("code") or b"")
        fn_name = co.get("name")

        if "__notifyevents__" in names:
            for c in consts:
                if isinstance(c, str) and (
                    c.startswith("On") or c.startswith("Do") or c.startswith("Process")
                ) and re.match(r"^[A-Za-z0-9_]+$", c):
                    notify_events[c].add(pyj_name)
                elif isinstance(c, list):
                    for item in c:
                        if isinstance(item, str) and (
                            item.startswith("On") or item.startswith("Do") or item.startswith("Process")
                        ) and re.match(r"^[A-Za-z0-9_]+$", item):
                            notify_events[item].add(pyj_name)

        for idx, (op, arg) in enumerate(insns):
            if op in (LOAD_ATTR, LOAD_GLOBAL, LOAD_NAME) and arg is not None and arg < len(names):
                nm = names[arg]
                if nm in ("RemoteSvc", "ProxySvc", "ConnectToRemoteService", "Moniker", "GetMoniker"):
                    if idx + 1 < len(insns):
                        op2, arg2 = insns[idx + 1]
                        if op2 == LOAD_CONST and arg2 is not None and arg2 < len(consts) and isinstance(consts[arg2], str):
                            svc_name = consts[arg2].strip()
                            if svc_name and re.match(r"^[A-Za-z][A-Za-z0-9_]{1,40}$", svc_name):
                                remote_services[svc_name]["files"].add(pyj_name)
                                for k in range(idx + 2, min(idx + 10, len(insns))):
                                    if insns[k][0] in CALL_OPS:
                                        for m in range(k + 1, min(k + 6, len(insns))):
                                            if insns[m][0] == STORE_ATTR and insns[m][1] < len(names):
                                                class_attr_svc[names[insns[m][1]]] = svc_name
                                            elif insns[m][0] == RETURN_VALUE and fn_name:
                                                class_attr_svc[fn_name] = svc_name
                                        break
                elif nm in MONIKER_HELPERS:
                    svc_name = MONIKER_HELPERS[nm]
                    remote_services[svc_name]["files"].add(pyj_name)
                    for k in range(idx + 1, min(idx + 8, len(insns))):
                        if insns[k][0] in CALL_OPS:
                            for m in range(k + 1, min(k + 6, len(insns))):
                                if insns[m][0] == STORE_ATTR and insns[m][1] < len(names):
                                    class_attr_svc[names[insns[m][1]]] = svc_name
                                elif insns[m][0] == RETURN_VALUE and fn_name:
                                    class_attr_svc[fn_name] = svc_name
                            break

        for c in consts:
            if isinstance(c, dict) and c.get("__code__"):
                collect_notify_and_pass1(c, pyj_name, class_attr_svc)

    def collect_methods_pass2(co, pyj_name, class_attr_svc):
        if not isinstance(co, dict) or not co.get("__code__"):
            return
        consts = co.get("consts") or []
        names = co.get("names") or []
        varnames = co.get("varnames") or []
        insns = disasm_py27(co.get("code") or b"")
        local_svc = {}

        for idx, (op, arg) in enumerate(insns):
            if op in (LOAD_ATTR, LOAD_GLOBAL, LOAD_NAME) and arg is not None and arg < len(names):
                nm = names[arg]
                if nm in ("RemoteSvc", "ProxySvc", "ConnectToRemoteService", "Moniker", "GetMoniker"):
                    if idx + 1 < len(insns):
                        op2, arg2 = insns[idx + 1]
                        if op2 == LOAD_CONST and arg2 is not None and arg2 < len(consts) and isinstance(consts[arg2], str):
                            svc_name = consts[arg2].strip()
                            if svc_name and re.match(r"^[A-Za-z][A-Za-z0-9_]{1,40}$", svc_name):
                                for k in range(idx + 2, min(idx + 8, len(insns))):
                                    if insns[k][0] in CALL_OPS and k + 1 < len(insns):
                                        op_n, arg_n = insns[k + 1]
                                        if op_n == LOAD_ATTR and arg_n < len(names):
                                            remote_services[svc_name]["candidate_methods"].add(names[arg_n])
                                        elif op_n == STORE_FAST and arg_n < len(varnames):
                                            local_svc[varnames[arg_n]] = svc_name
                                        break
                elif nm in MONIKER_HELPERS:
                    svc_name = MONIKER_HELPERS[nm]
                    for k in range(idx + 1, min(idx + 6, len(insns))):
                        if insns[k][0] in CALL_OPS and k + 1 < len(insns):
                            op_n, arg_n = insns[k + 1]
                            if op_n == LOAD_ATTR and arg_n < len(names):
                                remote_services[svc_name]["candidate_methods"].add(names[arg_n])
                            elif op_n == STORE_FAST and arg_n < len(varnames):
                                local_svc[varnames[arg_n]] = svc_name
                            break
                elif nm in class_attr_svc:
                    svc_name = class_attr_svc[nm]
                    if idx + 1 < len(insns):
                        op_n, arg_n = insns[idx + 1]
                        if op_n == LOAD_ATTR and arg_n < len(names):
                            remote_services[svc_name]["candidate_methods"].add(names[arg_n])
                        elif op_n == STORE_FAST and arg_n < len(varnames):
                            local_svc[varnames[arg_n]] = svc_name
                        elif op_n in CALL_OPS and idx + 2 < len(insns):
                            op_n2, arg_n2 = insns[idx + 2]
                            if op_n2 == LOAD_ATTR and arg_n2 < len(names):
                                remote_services[svc_name]["candidate_methods"].add(names[arg_n2])
                            elif op_n2 == STORE_FAST and arg_n2 < len(varnames):
                                local_svc[varnames[arg_n2]] = svc_name
            elif op == LOAD_FAST and arg is not None and arg < len(varnames):
                vn = varnames[arg]
                if vn in local_svc and idx + 1 < len(insns):
                    op_n, arg_n = insns[idx + 1]
                    if op_n == LOAD_ATTR and arg_n < len(names):
                        remote_services[local_svc[vn]]["candidate_methods"].add(names[arg_n])

        for c in consts:
            if isinstance(c, dict) and c.get("__code__"):
                collect_methods_pass2(c, pyj_name, class_attr_svc)

    with zipfile.ZipFile(code_ccp_path, "r") as zf:
        for name in zf.namelist():
            if name.startswith("eveProto/generated/") and name.endswith("_pb2.pyj"):
                rel = name[len("eveProto/generated/") :]
                parts = rel.split("/")
                if parts[0] == "eve_public":
                    domain = parts[1] if len(parts) > 2 else "eve_public_root"
                else:
                    domain = parts[1] if len(parts) > 2 else parts[0]
                protobuf_domains[domain].append(name)
                continue
            elif name.startswith("eveProto/"):
                continue

            try:
                raw = zlib.decompress(zf.read(name))
                if raw[:4] != b"\x03\xf3\x0d\x0a":
                    continue
                co = Py27MarshalReader(raw[8:]).load()
                class_attr_svc = {}
                collect_notify_and_pass1(co, name, class_attr_svc)
                collect_methods_pass2(co, name, class_attr_svc)
            except Exception:
                continue

    return remote_services, notify_events, protobuf_domains


def scan_evomu_router(router_rs_path):
    """Extracts currently implemented `(service, method)` pairs from `crates/eve-server/src/router.rs`."""
    implemented = collections.defaultdict(set)
    if not os.path.isfile(router_rs_path):
        return implemented
    with open(router_rs_path, "r", encoding="utf-8") as f:
        content = f.read()
    for svc, method in re.findall(r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)', content):
        canonical_svc = "authentication" if svc == "auth" else svc
        implemented[canonical_svc].add(method)
    return implemented


def scan_staticdata_index(resfileindex_gz_path):
    """Extracts all `res:/staticdata/` and `res:/localizationfsd/` entries from `resfileindex.txt`."""
    entries = []
    if not os.path.isfile(resfileindex_gz_path):
        return entries
    with gzip.open(resfileindex_gz_path, "rt", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line.startswith(("res:/staticdata/", "res:/localizationfsd/")):
                parts = line.split(",")
                if len(parts) >= 5:
                    entries.append({
                        "res_path": parts[0],
                        "cdn_relative_path": parts[1],
                        "md5": parts[2],
                        "uncompressed_bytes": int(parts[3]),
                        "compressed_bytes": int(parts[4]),
                        "format": "SQLite3" if parts[0].endswith(".static") else (
                            "FSD Binary" if parts[0].endswith(".fsdbinary") else "Pickle/Binary"
                        ),
                    })
    return sorted(entries, key=lambda x: x["res_path"])


def scan_eve_wiki(eve_wiki_root):
    """Scans `~/ai/eve-wiki` (`catalog.json` and `articles/*.md`) for formulas, ship specs, and PvE/mechanics articles."""
    catalog_path = os.path.join(eve_wiki_root, "catalog.json")
    if not os.path.isfile(catalog_path):
        return {}

    with open(catalog_path, "r", encoding="utf-8") as f:
        cat = json.load(f)

    articles = cat.get("articles", {})
    formulas_by_article = {}
    categories_index = collections.defaultdict(list)
    ship_specs_count = 0

    for slug, meta in sorted(articles.items()):
        title = meta.get("title", slug)
        for c in meta.get("categories", []):
            categories_index[c].append(title)
        if meta.get("has_specs"):
            ship_specs_count += 1
        if meta.get("has_formulas"):
            art_path = os.path.join(eve_wiki_root, meta.get("article_path", ""))
            extracted = []
            if os.path.isfile(art_path):
                with open(art_path, "r", encoding="utf-8", errors="replace") as af:
                    text = af.read()
                for eq in re.findall(r"\$\$(.*?)\$\$", text, flags=re.DOTALL):
                    clean_eq = " ".join(eq.strip().split())
                    if clean_eq:
                        extracted.append(clean_eq)
            formulas_by_article[title] = {
                "article_path": meta.get("article_path", ""),
                "wikitext_path": meta.get("wikitext_path", ""),
                "page_id": meta.get("page_id"),
                "formula_count": len(extracted),
                "formulas": extracted,
            }

    key_pve_categories = [
        "Security missions",
        "COSMOS missions",
        "COSMOS agents",
        "COSMOS sites",
        "Epic arc missions",
        "Epic Arcs",
        "Combat Anomalies",
        "Storyline missions",
        "Expeditions",
        "Unrated Complexes",
        "Distribution Missions",
        "Wormhole sites",
        "Gas sites",
        "Ore sites",
        "DED Complexes",
        "Data center missions",
        "Data center agents",
        "Mining Missions",
        "Circle missions",
        "Chemical Labs",
        "Data sites",
        "Relic sites",
        "Sovereignty Hub ore deposits",
        "Incursions sites",
        "Incursions",
        "Anomic missions",
        "Homefront Operations",
        "Ice belts",
        "Mercenary Tactical Operations",
        "Game mechanics",
        "Structures",
        "Weapons",
        "Faction Warfare",
        "Planetary Industry",
    ]

    curated_categories = {
        c: {
            "count": len(categories_index.get(c, [])),
            "articles": sorted(categories_index.get(c, [])),
        }
        for c in key_pve_categories
        if c in categories_index
    }

    return {
        "summary": {
            "total_articles": len(articles),
            "formula_articles_count": len(formulas_by_article),
            "total_extracted_formulas": sum(v["formula_count"] for v in formulas_by_article.values()),
            "ship_specs_articles_count": ship_specs_count,
            "total_unique_categories": len(categories_index),
        },
        "formulas_by_article": formulas_by_article,
        "curated_categories": curated_categories,
    }


def main():
    parser = argparse.ArgumentParser(description="EvoMu Exhaustive Server Logic Indexer")
    parser.add_argument("--crucible", default=os.environ.get("EVOMU_CRUCIBLE_DIR", ""), help="Path to evemu_Crucible checkout")
    parser.add_argument("--code-ccp", default=os.environ.get("EVOMU_CODE_CCP", ""), help="Path to V24.01 code.ccp")
    parser.add_argument("--resfileindex", default=os.environ.get("EVOMU_RESFILEINDEX_GZ", ""), help="Path to V24.01 resfileindex.gz")
    parser.add_argument("--eve-wiki", default=os.environ.get("EVOMU_WIKI_DIR", os.path.expanduser("~/ai/eve-wiki")), help="Path to local eve-wiki clone")
    parser.add_argument("--evomu-root", default=os.path.abspath(os.path.join(os.path.dirname(__file__), "../..")))
    args = parser.parse_args()

    out_dir = os.path.join(args.evomu_root, "docs", "index")
    os.makedirs(out_dir, exist_ok=True)

    router_path = os.path.join(args.evomu_root, "crates", "eve-server", "src", "router.rs")
    evomu_impl = scan_evomu_router(router_path)
    crucible_svcs = scan_crucible_services(args.crucible) if args.crucible else {}
    v24_svcs, notify_events, pb_domains = scan_v24_code_ccp(args.code_ccp) if args.code_ccp else ({}, {}, {})
    staticdata_entries = scan_staticdata_index(args.resfileindex) if args.resfileindex else []
    wiki_catalog = scan_eve_wiki(args.eve_wiki) if args.eve_wiki and os.path.isdir(args.eve_wiki) else {}
    dogma_effects, destiny_opcodes = scan_crucible_dogma_and_destiny(args.crucible, args.code_ccp)

    all_service_names = sorted(set(crucible_svcs.keys()) | set(v24_svcs.keys()) | set(evomu_impl.keys()))
    service_catalog = {}
    v24_active_count = 0
    crucible_legacy_only_count = 0

    for svc in all_service_names:
        c_info = crucible_svcs.get(svc, {"cpp_classes": set(), "methods": {}})
        v_info = v24_svcs.get(svc, {"files": set(), "candidate_methods": set()})
        e_methods = sorted(evomu_impl.get(svc, set()))
        has_v24 = len(v_info["files"]) > 0
        if has_v24:
            v24_active_count += 1
            surface_cat = "v24_active"
        else:
            crucible_legacy_only_count += 1
            surface_cat = "crucible_legacy_only"

        service_catalog[svc] = {
            "surface_category": surface_cat,
            "evemu_crucible_classes": sorted(c_info["cpp_classes"]),
            "evemu_crucible_methods": c_info["methods"],
            "v24_client_call_files": sorted(v_info["files"]),
            "v24_client_candidate_methods": sorted(v_info["candidate_methods"]),
            "evomu_implemented_methods": e_methods,
        }

    with open(os.path.join(out_dir, "rpc_service_catalog.json"), "w", encoding="utf-8") as f:
        json.dump(service_catalog, f, indent=2, sort_keys=True)

    pb_catalog = {k: sorted(v) for k, v in sorted(pb_domains.items())}
    eve_public_domains = sorted([
        k for k, files in pb_catalog.items()
        if any(f.startswith("eveProto/generated/eve_public/") for f in files)
    ])
    eve_internal_domains = sorted([
        k for k in pb_catalog
        if k not in eve_public_domains
    ])

    crucible_comparison = {
        "summary": {
            "total_catalog_services": len(service_catalog),
            "v24_active_services": v24_active_count,
            "crucible_legacy_only_services": crucible_legacy_only_count,
            "both_v24_and_crucible": sum(
                1 for s in service_catalog.values()
                if s["v24_client_call_files"] and s["evemu_crucible_classes"]
            ),
            "v24_only_new_services": sum(
                1 for s in service_catalog.values()
                if s["v24_client_call_files"] and not s["evemu_crucible_classes"]
            ),
            "evomu_services_touched": len(evomu_impl),
            "evomu_rpc_methods_implemented": sum(len(m) for m in evomu_impl.values()),
            "quasar_protobuf_domains_total": len(pb_catalog),
            "quasar_eve_public_domains_count": len(eve_public_domains),
            "quasar_eve_internal_domains_count": len(eve_internal_domains),
            "quasar_pb2_files_count": sum(len(v) for v in pb_catalog.values()),
        },
        "v24_only_services": sorted([
            k for k, s in service_catalog.items()
            if s["v24_client_call_files"] and not s["evemu_crucible_classes"]
        ]),
        "both_v24_and_crucible_services": sorted([
            k for k, s in service_catalog.items()
            if s["v24_client_call_files"] and s["evemu_crucible_classes"]
        ]),
        "crucible_legacy_only_services": sorted([
            k for k, s in service_catalog.items()
            if not s["v24_client_call_files"] and s["evemu_crucible_classes"]
        ]),
        "quasar_eve_public_domains": eve_public_domains,
        "quasar_eve_internal_domains": eve_internal_domains,
    }
    with open(os.path.join(out_dir, "crucible_comparison.json"), "w", encoding="utf-8") as f:
        json.dump(crucible_comparison, f, indent=2, sort_keys=True)

    notify_catalog = {k: sorted(v) for k, v in sorted(notify_events.items())}
    with open(os.path.join(out_dir, "client_notifications.json"), "w", encoding="utf-8") as f:
        json.dump(notify_catalog, f, indent=2, sort_keys=True)

    pb_catalog = {k: sorted(v) for k, v in sorted(pb_domains.items())}
    with open(os.path.join(out_dir, "quasar_protobuf_catalog.json"), "w", encoding="utf-8") as f:
        json.dump(pb_catalog, f, indent=2, sort_keys=True)

    with open(os.path.join(out_dir, "staticdata_catalog.json"), "w", encoding="utf-8") as f:
        json.dump(staticdata_entries, f, indent=2)

    with open(os.path.join(out_dir, "dogma_effects.json"), "w", encoding="utf-8") as f:
        json.dump(dogma_effects, f, indent=2, sort_keys=True)

    with open(os.path.join(out_dir, "destiny_opcodes.json"), "w", encoding="utf-8") as f:
        json.dump(destiny_opcodes, f, indent=2, sort_keys=True)

    if wiki_catalog:
        with open(os.path.join(out_dir, "wiki_mechanics_catalog.json"), "w", encoding="utf-8") as f:
            json.dump(wiki_catalog, f, indent=2, sort_keys=True)

    print(
        "[OK] Indexed %d total services (%d V24.01 active, %d Crucible legacy-only), %d __notifyevents__, %d Quasar Protobuf domains (%d _pb2.pyj files), %d staticdata archives, %d Dogma effects, %d Destiny opcodes, and %d wiki articles into %s"
        % (
            len(service_catalog),
            v24_active_count,
            crucible_legacy_only_count,
            len(notify_catalog),
            len(pb_catalog),
            sum(len(v) for v in pb_catalog.values()),
            len(staticdata_entries),
            dogma_effects["effects_count"],
            len(destiny_opcodes["xmlp_elements"]),
            wiki_catalog.get("summary", {}).get("total_articles", 0),
            out_dir,
        )
    )


if __name__ == "__main__":
    main()
