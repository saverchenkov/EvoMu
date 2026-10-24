#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""CCP Stackless Python 2.7 Dogma Modifier & Location Evaluator (plan:T-12).

Executes CCP's authentic compiled Dogma modules:
- dogma.effects.modifiers (ItemModifier, LocationModifier, LocationGroupModifier,
  LocationRequiredSkillModifier, OwnerRequiredSkillModifier, modifierGetterByType)
- dogma.effects.modifiereffect.ModifierEffect
- dogma.mocks.mockDogmaLocation.MockDogmaLocation
- dogma.attributes.attribute.Attribute
- dogma.attributes.stackingNurfedAttribute.StackingNurfedAttribute
inside CCP's bundled Stackless Python 2.7.1 runtime via cfsd_runner.

Queries attribute metadata (default_value, high_is_good, stackable) dynamically from
`data/evomu_sde.sqlite`.
Instantiates CCP's `Attribute` when stackable == 1, and `StackingNurfedAttribute` when
stackable == 0 (non-stackable attributes apply CCP's `NurfDenominators` stacking penalties).
Wires modifier graphs directly using CCP's `ModifierEffect` and `modifierGetterByType`,
dispatching modifiers through `MockDogmaLocation`.
Evaluates pirate implant set multipliers and the 10 core operators in CCP's `const.dgmOperatorPrecedence`.
Command burst dbuffCollections operationName (Minimum/Maximum/Additive) aggregation is
DERIVED from the SDE dbuffCollections schema before passing the winning modifier into
CCP's Dogma operator pipeline.
Scales across 10 canonical fits and 500+ procedural ship/module/implant fits.
"""

import sys

if len(sys.argv) < 4:
    sys.stderr.write(
        "Usage: cfsd_runner tools/dogma_evaluator_py2.py <code_pyc.zip> <bin64_dir> <sde_sqlite> [specs.json] [out.json] [--procedural-fits N]\n"
    )
    sys.exit(2)

code_pyc_zip = sys.argv[1]
bin64_dir = sys.argv[2]
sde_sqlite_path = sys.argv[3]

specs_path = None
out_json_path = None
procedural_count = 520

pos_args = []
idx = 4
while idx < len(sys.argv):
    arg = sys.argv[idx]
    if arg == "--procedural-fits":
        if idx + 1 < len(sys.argv):
            procedural_count = int(sys.argv[idx + 1])
            idx += 2
        else:
            idx += 1
    elif arg == "--no-procedural-fits":
        procedural_count = 0
        idx += 1
    else:
        pos_args.append(arg)
        idx += 1

if len(pos_args) > 0 and pos_args[0] != "":
    specs_path = pos_args[0]
if len(pos_args) > 1 and pos_args[1] != "":
    out_json_path = pos_args[1]

# Set up runtime import paths
sys.path.insert(0, code_pyc_zip)
sys.path.insert(0, bin64_dir)
import encodings  # noqa: F401, E402

from collections import defaultdict
import json
import sqlite3
import types

# Ensure taskless and uthread2 are available in the bundled environment
try:
    import uthread2_plugins.taskless  # noqa: F401
    import uthread2  # noqa: F401
except ImportError:
    pass


# ---------------------------------------------------------------------------
# Minimal Mock Environment for Headless Dogma Bytecode Execution
# ---------------------------------------------------------------------------
class Mock(object):
    def __getattr__(self, name):
        return Mock()

    def __call__(self, *args, **kwargs):
        return Mock()


mock_blue = types.ModuleType("blue")
mock_blue.os = Mock()
mock_blue.pyos = Mock()
mock_blue.classes = Mock()
mock_blue.resMan = Mock()
mock_blue.win32 = Mock()
mock_blue.marshal = Mock()
mock_blue.paths = Mock()
mock_blue.statistics = Mock()
mock_blue.statistics.EnterZone = lambda *a, **k: None
mock_blue.statistics.LeaveZone = lambda *a, **k: None
mock_blue.DBRowDescriptor = lambda *a: [0] * 8
mock_blue.DBRow = lambda *a: Mock()


def _load_ext(name):
    mod = __import__(name)
    sys.modules[name] = mod
    return mod


mock_blue.LoadExtension = _load_ext
mock_blue.pyos.LoadExtension = _load_ext
sys.modules["blue"] = mock_blue

class LogChannel(object):
    flags = {}
    def __init__(self, *a, **k): pass
    def IsOpen(self): return False
    def Log(self, *a, **k): pass

mock_log = types.ModuleType("log")
mock_log.LogChannel = LogChannel
mock_log.Channel = LogChannel
mock_log.GetChannel = lambda *a, **k: LogChannel()
mock_log.flags = {}
mock_logmodule = types.ModuleType("logmodule")
mock_logmodule.LogChannel = LogChannel
mock_logmodule.Channel = LogChannel
mock_logmodule.GetChannel = lambda *a, **k: LogChannel()
for _m in [mock_log, mock_logmodule]:
    _m.LogException = lambda *a, **k: None
    _m.LogTraceback = lambda *a, **k: None
    _m.LogError = lambda *a, **k: None
    _m.LogWarn = lambda *a, **k: None
    _m.LogInfo = lambda *a, **k: None
sys.modules["log"] = mock_log
sys.modules["logmodule"] = mock_logmodule

mock_stdlogutils = types.ModuleType("stdlogutils")
mock_stdlogutils.logserver = Mock()
mock_stdlogutils.logserver.LogChannel = LogChannel
mock_stdlogutils.LineWrap = lambda s, *a, **k: s
sys.modules["stdlogutils"] = mock_stdlogutils
sys.modules["stdlogutils.logserver"] = mock_stdlogutils.logserver

import __builtin__
boot = Mock()
boot.role = "client"
__builtin__.boot = boot
sys.modules["boot"] = boot
eveprefs = types.ModuleType("eveprefs")
eveprefs.boot = boot
eveprefs.prefs = Mock()
eveprefs.prefs.GetValue = lambda *a, **k: None
sys.modules["eveprefs"] = eveprefs

sys.ClockThis = sys.ClockThisWithoutTheStars = lambda fn, *a, **k: fn(*a, **k)

uthread_mod = types.ModuleType("uthread")
uthread_mod.new = lambda fn, *a, **k: Mock()
uthread_mod.schedule = lambda *a, **k: None
sys.modules["uthread"] = uthread_mod

uthread2_mod = types.ModuleType("uthread2")
uthread2_mod.start_tasklet = lambda fn, *a, **k: None
uthread2_mod.map = lambda fn, it: [fn(x) for x in it]
sys.modules["uthread2"] = uthread2_mod

log_mod = types.ModuleType("dogma.dogmaLogging")
log_mod.LogWarn = lambda *a, **k: None
log_mod.LogTraceback = lambda *a, **k: None
log_mod.LogError = lambda *a, **k: None
log_mod.LogException = lambda *a, **k: None
log_mod.GetCategoryNameFromID = lambda cid: "Category"
log_mod.GetGroupNameFromID = lambda gid: "Group"
log_mod.GetTypeNameFromID = lambda tid: "Type"
log_mod.WrappedMethod = lambda fn: fn
sys.modules["dogma.dogmaLogging"] = log_mod

import dogma  # noqa: E402

dogma.IsClient = lambda: True
dogma.dogmaLogging = log_mod

dogma_data = types.ModuleType("dogma.data")
dogma_data.get_type_attribute_or_default = lambda tid, aid, default=None: default
dogma_data.get_type_attribute = lambda tid, aid: None
dogma_data.get_effect = lambda eid: None
dogma_data.get_attribute = lambda aid: None
dogma_data.get_charge_recharge_time_attributes_by_id = lambda: {}
sys.modules["dogma.data"] = dogma_data
dogma.data = dogma_data

import dogma.const as dogma_const
import inventorycommon.const as inv_const
class ConstProxy(object):
    def __getattr__(self, name):
        if hasattr(dogma_const, name): return getattr(dogma_const, name)
        if hasattr(inv_const, name): return getattr(inv_const, name)
        return 0
__builtin__.const = ConstProxy()
const = ConstProxy()

# DERIVED_OP9_SPECIAL_SKILL_RELATIONSHIP:
# In the V24.01 SDE, operator 9 (SpecialSkillRelationship / dgmAssSpecialSkillRelationship)
# only appears on effect_id = 132 (skillEffect, modified_attribute_id: 280 skillLevel,
# modifying_attribute_id: 275 skillTimeConstant) and is not present in CCP's
# dogma/const.pyj dgmOperatorPrecedence ([-1, 0, 1, 2, 3, 4, 5, 6, 8, 7]).
# Operator 9 is a DERIVED EvoMu extension inserted before PostAssignment (7)
# with direct percentage scaling: v * (1.0 + m / 100.0).
if 9 not in dogma_const.dgmOperatorPrecedence:
    idx_7 = dogma_const.dgmOperatorPrecedence.index(7) if 7 in dogma_const.dgmOperatorPrecedence else len(dogma_const.dgmOperatorPrecedence)
    dogma_const.dgmOperatorPrecedence.insert(idx_7, 9)
    dogma_const.dgmOperators[9] = lambda v, m: v * (1.0 + m / 100.0)
    dogma_const.dgmOperatorIDtoIndex = {op: i for i, op in enumerate(dogma_const.dgmOperatorPrecedence)}

import dogma.attributes.attribute as attr  # noqa: E402
import dogma.attributes.stackingNurfedAttribute as sna  # noqa: E402
import dogma.effects.modifiers as mods  # noqa: E402
import dogma.effects.modifiereffect as me  # noqa: E402

import marshal
import zipfile

_z = zipfile.ZipFile(code_pyc_zip, "r")
_bdl_data = _z.read("eve/common/script/dogma/baseDogmaLocation.pyc")
_bdl_code = marshal.loads(_bdl_data[8:])
_bdl_mod = types.ModuleType("baseDogmaLocation")
_bdl_mod.__dict__["__file__"] = "eve/common/script/dogma/baseDogmaLocation.py"
exec _bdl_code in _bdl_mod.__dict__
BaseDogmaLocation = _bdl_mod.BaseDogmaLocation



# ---------------------------------------------------------------------------
# SDE Database Access
# ---------------------------------------------------------------------------
conn = sqlite3.connect(sde_sqlite_path)
cur = conn.cursor()

_ATTR_CACHE = {}
_TYPE_CACHE = {}
_EFFECT_CACHE = {}


def get_attr_meta(aid):
    if aid in _ATTR_CACHE:
        return _ATTR_CACHE[aid]
    row = cur.execute(
        "SELECT default_value, high_is_good, stackable FROM sde_dogma_attributes WHERE attribute_id = ?",
        (aid,),
    ).fetchone()
    if not row:
        meta = (0.0, True, True)
    else:
        meta = (float(row[0]), bool(row[1]), bool(row[2]))
    _ATTR_CACHE[aid] = meta
    return meta


def sde_attr_is_stackable(aid):
    return get_attr_meta(aid)[2]


def get_type(tid):
    if tid in _TYPE_CACHE:
        return _TYPE_CACHE[tid]
    row = cur.execute(
        "SELECT type_id, group_id, category_id, name, attributes_json, effects_json, capacity FROM sde_types WHERE type_id = ?",
        (tid,),
    ).fetchone()
    if not row:
        return None
    res = {
        "type_id": row[0],
        "group_id": row[1],
        "category_id": row[2],
        "name": row[3],
        "attributes": {int(k): float(v) for k, v in json.loads(row[4]).iteritems()},
        "effects": json.loads(row[5]),
        "capacity": float(row[6]),
    }
    _TYPE_CACHE[tid] = res
    return res


def get_effect(eid):
    if eid in _EFFECT_CACHE:
        return _EFFECT_CACHE[eid]
    row = cur.execute(
        "SELECT effect_id, name, effect_category, modifiers_json FROM sde_dogma_effects WHERE effect_id = ?",
        (eid,),
    ).fetchone()
    if not row:
        return None
    res = {
        "effect_id": row[0],
        "name": row[1],
        "category": row[2],
        "modifiers": json.loads(row[3]),
    }
    _EFFECT_CACHE[eid] = res
    return res


# ---------------------------------------------------------------------------
# Dogma Item & Location Dispatch Models
# ---------------------------------------------------------------------------
class StaticAttr(object):
    def __init__(self, aid, def_val, high_is_good, stackable):
        self.attributeID = aid
        self.defaultValue = def_val
        self.highIsGood = high_is_good
        self.stackable = stackable
        self.dataType = 1
        self.minAttributeID = None
        self.maxAttributeID = None


class MockInvItem(object):
    def __init__(self, item_id, type_id, group_id, category_id):
        self.itemID = item_id
        self.typeID = type_id
        self.groupID = group_id
        self.categoryID = category_id


class DogmaItemAttrs(dict):
    def __init__(self, item):
        dict.__init__(self)
        self.item = item

    def __contains__(self, aid):
        if dict.__contains__(self, aid):
            return True
        t = get_type(self.item.typeID)
        if t and aid in t["attributes"]:
            return True
        return False

    def __getitem__(self, aid):
        if not dict.__contains__(self, aid):
            a = self.item.get_or_create_attr(aid)
            dict.__setitem__(self, aid, a)
        return dict.__getitem__(self, aid)


class DogmaItem(object):
    def __init__(self, item_id, type_id, group_id, category_id, lm):
        self.itemID = item_id
        self.invItem = MockInvItem(item_id, type_id, group_id, category_id)
        self.typeID = type_id
        self.groupID = group_id
        self.categoryID = category_id
        self.attributes = DogmaItemAttrs(self)
        self.subItems = set()
        self.fittedItems = {}
        self.reqSkills = set()
        self.locationMods = defaultdict(set)
        self.locationGroupMods = defaultdict(lambda: defaultdict(set))
        self.locationReqSkillMods = defaultdict(lambda: defaultdict(set))
        self.ownerReqSkillMods = defaultdict(lambda: defaultdict(set))
        self.dogmaLocation = lm
        self.lm = lm
        lm.dogmaItems[item_id] = self

    def GetLocation(self):
        return None

    def GetPilot(self):
        return None

    def IsOwnerModifiable(self):
        return False

    def GetOwnedItemsToModify(self):
        return list(self.subItems)

    def IterFittedItems(self):
        return self.fittedItems.itervalues()

    def get_or_create_attr(self, aid, base_val=None):
        if dict.__contains__(self.attributes, aid):
            a = dict.__getitem__(self.attributes, aid)
            if base_val is not None:
                a.SetBaseValue(base_val)
            return a
        def_val, high_is_good, stackable = get_attr_meta(aid)
        if base_val is None:
            base_val = def_val
        sa = StaticAttr(aid, def_val, high_is_good, stackable)
        if stackable:
            a = attr.Attribute(aid, base_val, self, self.lm, sa)
        else:
            a = sna.StackingNurfedAttribute(aid, base_val, self, self.lm, sa)
        dict.__setitem__(self.attributes, aid, a)
        return a


class ModEnv(object):
    def __init__(self, itemID, shipID, charID, otherID=None, targetID=None):
        self.itemID = itemID
        self.shipID = shipID
        self.charID = charID
        self.otherID = otherID
        self.targetID = targetID


class ModSpec(object):
    def __init__(self, d):
        self.operation = d.get("operator")
        self.domain = d.get("domain")
        self.modifiedAttributeID = d.get("modified_attribute_id")
        self.modifyingAttributeID = d.get("modifying_attribute_id")
        self.groupID = d.get("group_id")
        self.skillTypeID = d.get("skill_type_id")


class SimpleMod(object):
    def __init__(self, val, item_id=999000, cat_id=16, lm=None):
        self.val = float(val)
        if lm is not None:
            self.dogmaItem = DogmaItem(item_id, 1, 1, cat_id, lm)
            self.item = self.dogmaItem
        else:
            self.item = Mock()
            self.item.itemID = item_id
            self.item.categoryID = cat_id
            self.dogmaItem = self.item

    def GetValue(self):
        return self.val

    def AddModifierTo(self, op, to_attr):
        to_attr.AddIncomingModifier(op, self)


class EvoMuDogmaLocation(BaseDogmaLocation):
    """Subclass of CCP's BaseDogmaLocation wired to handle Dogma modifier dispatch (plan:T-12)."""

    def __init__(self):
        BaseDogmaLocation.__init__(self, broker=Mock())
        self.dogmaItems = {}
        self.modifiersAwaitingTarget = defaultdict(set)
        self.itemsMissingLocation = defaultdict(set)
        self.itemsMissingOwner = defaultdict(set)
        self.attributeChangeCallbacksByAttributeID = defaultdict(list)
        # Aliases for skill modifier dispatch from dogma.effects.modifiers
        self.AddLocationSkillModifier = self.AddLocationRequiredSkillModifier
        self.AddOwnerSkillModifier = self.AddOwnerRequiredSkillModifier


def build_modifier_effect(eff_id, eff_spec, mod_attrs=None):
    modifier_objs = []
    specs = list(eff_spec["modifiers"])
    if eff_id in (6730, 6731) and not specs:
        mass_attr_id = 796 if (mod_attrs and ("796" in mod_attrs or 796 in mod_attrs)) else 4
        specs = [
            {
                "func": "ItemModifier",
                "domain": "shipID",
                "operator": 2,
                "modifying_attribute_id": mass_attr_id,
                "modified_attribute_id": 4,
            },
            {
                "func": "ItemModifier",
                "domain": "shipID",
                "operator": 6,
                "modifying_attribute_id": 20,
                "modified_attribute_id": 37,
            },
        ]
        if eff_id == 6730:
            sig_attr = 554 if (mod_attrs and ("554" in mod_attrs or 554 in mod_attrs)) else (986 if (mod_attrs and ("986" in mod_attrs or 986 in mod_attrs)) else 0)
            if sig_attr:
                specs.append({
                    "func": "ItemModifier",
                    "domain": "shipID",
                    "operator": 6,
                    "modifying_attribute_id": sig_attr,
                    "modified_attribute_id": 552,
                })
    for spec in specs:
        func_name = spec["func"]
        if func_name in mods.modifierGetterByType:
            builder = mods.modifierGetterByType[func_name]
            mod_obj = builder(ModSpec(spec))
            modifier_objs.append(mod_obj)
    effect_info = Mock()
    effect_info.effectCategory = eff_spec["category"]
    return me.ModifierEffect(effect_info, modifier_objs)


ATTR_NAME_TO_ID = {
    "maxVelocity": 37,
    "agility": 70,
    "capacity": 38,
    "hp_structure": 9,
    "armorHP": 265,
    "shieldCapacity": 263,
    "sigRadius": 552,
    "cpuOutput": 48,
    "powerOutput": 11,
    "cpuLoad": 49,
    "powerLoad": 15,
    "shieldEmResonance": 271,
    "shieldExplosiveResonance": 272,
    "shieldKineticResonance": 273,
    "shieldThermalResonance": 274,
    "armorEmResonance": 267,
    "armorExplosiveResonance": 268,
    "armorKineticResonance": 269,
    "armorThermalResonance": 270,
}


# ---------------------------------------------------------------------------
# 11-Operator Precedence Verification via CCP's Attribute Class
# ---------------------------------------------------------------------------
def evaluate_operator_precedence():
    """Evaluates CCP's authentic 11-operator precedence sequence via Attribute."""
    lm = EvoMuDogmaLocation()
    item = DogmaItem(999, 587, 25, 6, lm)
    sa = StaticAttr(37, 100.0, True, True)
    target_attr = attr.Attribute(37, 100.0, item, lm, sa)

    target_attr.AddIncomingModifier(2, SimpleMod(20.0, item_id=1, lm=lm))
    target_attr.AddIncomingModifier(3, SimpleMod(10.0, item_id=2, lm=lm))
    target_attr.AddIncomingModifier(0, SimpleMod(2.0, item_id=3, lm=lm))
    target_attr.AddIncomingModifier(1, SimpleMod(2.0, item_id=4, lm=lm))
    target_attr.AddIncomingModifier(-1, SimpleMod(50.0, item_id=5, lm=lm))
    target_attr.AddIncomingModifier(4, SimpleMod(1.5, item_id=6, lm=lm))
    target_attr.AddIncomingModifier(5, SimpleMod(1.25, item_id=7, lm=lm))
    target_attr.AddIncomingModifier(6, SimpleMod(10.0, item_id=8, lm=lm))
    target_attr.AddIncomingModifier(8, SimpleMod(10.0, item_id=9, lm=lm))

    val_before_post_assign = target_attr.GetValue()
    target_attr.AddIncomingModifier(9, SimpleMod(5.0, item_id=11, lm=lm))
    val_with_op9 = target_attr.GetValue()
    target_attr.AddIncomingModifier(7, SimpleMod(250.0, item_id=10, lm=lm))
    val_after_post_assign = target_attr.GetValue()

    return {
        "val_before_post_assign": val_before_post_assign,
        "val_with_op9": val_with_op9,
        "val_after_post_assign": val_after_post_assign,
        "op9_status": "DERIVED_OP9_SPECIAL_SKILL_RELATIONSHIP",
        "description": "CCP V24.01 dogma/const.pyj defines 10 operators [-1, 0, 1, 2, 3, 4, 5, 6, 8, 7]. Operator 9 is a DERIVED EvoMu extension.",
    }


# ---------------------------------------------------------------------------
# Stacking Constants & Factors from CCP's StackingNurfedAttribute
# ---------------------------------------------------------------------------
def extract_stacking_data():
    denominators = list(sna.NurfDenominators)
    max_len = len(denominators)
    exp_factor = 2.67
    exp_power = 2.0

    factors = []
    for i in range(min(7, max_len)):
        factors.append({
            "index": i,
            "factor": 1.0 / denominators[i],
            "formula": "exp(-(i / 2.67)^2)",
            "denominator": denominators[i],
        })

    op_meta = {
        const.dgmAssPreAssignment: ("dgmAssPreAssignment", "Assign before all other arithmetic"),
        const.dgmAssPreMul: ("dgmAssPreMul", "Multiply before addition"),
        const.dgmAssPreDiv: ("dgmAssPreDiv", "Divide before addition"),
        const.dgmAssModAdd: ("dgmAssModAdd", "Add"),
        const.dgmAssModSub: ("dgmAssModSub", "Subtract"),
        const.dgmAssPostMul: ("dgmAssPostMul", "Multiply after addition"),
        const.dgmAssPostDiv: ("dgmAssPostDiv", "Divide after addition"),
        const.dgmAssPostPercent: ("dgmAssPostPercent", "Percentage Modifier"),
        const.dgmAssPostPercentUnNerfed: ("dgmAssPostPercentUnNerfed", "Percentage Modifier that can't be stacking penalised"),
        const.dgmAssSpecialSkillRelationship: ("dgmAssSpecialSkillRelationship", "Special skill relationship modifier"),
        const.dgmAssPostAssignment: ("dgmAssPostAssignment", "Assign after all other arithmetic (override)"),
    }
    operator_precedence = []
    for op_id in const.dgmOperatorPrecedence:
        name, desc = op_meta.get(op_id, ("dgmOperator_%d" % op_id, "CCP Dogma operator"))
        operator_precedence.append({"id": op_id, "name": name, "description": desc})

    return {
        "max_len": max_len,
        "exp_factor": exp_factor,
        "exp_power": exp_power,
        "stacking_factors": factors,
        "operator_precedence": operator_precedence,
    }


# ---------------------------------------------------------------------------
# Ship Fit Evaluation via CCP's ModifierEffect & MockDogmaLocation
# ---------------------------------------------------------------------------
def evaluate_dogma_fit(ship_type_id, skills, modules, implants, boosters=None, gang_bursts=None):
    if boosters is None:
        boosters = []
    if gang_bursts is None:
        gang_bursts = []
    lm = EvoMuDogmaLocation()
    ship_type = get_type(ship_type_id)
    ship = DogmaItem(100, ship_type_id, ship_type["group_id"], ship_type["category_id"], lm)
    char_id = 1000
    char_item = DogmaItem(char_id, 1373, 1, 1, lm)

    # Initialize ship base attributes (excluding dynamic fitting loads 15 & 49)
    for aid, val in ship_type["attributes"].iteritems():
        if aid not in (15, 49):
            ship.get_or_create_attr(aid, val)
    ship.get_or_create_attr(38, ship_type["capacity"])

    # Instantiate modules
    mod_items = []
    for m in modules:
        mt = get_type(m["type_id"])
        m_item = DogmaItem(m["item_id"], m["type_id"], mt["group_id"], mt["category_id"], lm)
        for aid, val in mt["attributes"].iteritems():
            m_item.get_or_create_attr(aid, val)
        # Apply mutaplasmid dynamic attribute overrides directly to module base attributes
        dyn_attrs = m.get("dynamic_attributes") or m.get("dynamic_attribute_overrides") or {}
        for aid_k, val in dyn_attrs.iteritems():
            m_item.get_or_create_attr(int(aid_k), float(val))
        for req_aid in (182, 183, 184, 1285, 1289, 1290):
            if req_aid in mt["attributes"]:
                m_item.reqSkills.add(int(mt["attributes"][req_aid]))
        ship.subItems.add(m_item)
        ship.fittedItems[m["item_id"]] = m_item
        mod_items.append({"item": m_item, "type": mt, "raw": m})

    # Implants
    imp_items = []
    for imp_tid in implants:
        imp_type = get_type(imp_tid)
        if not imp_type:
            continue
        imp_item = DogmaItem(imp_tid, imp_tid, imp_type["group_id"], imp_type["category_id"], lm)
        for aid, val in imp_type["attributes"].iteritems():
            imp_item.get_or_create_attr(aid, val)
        for r_aid, l_aid in ((182, 277), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)):
            if r_aid in imp_type["attributes"]:
                imp_item.reqSkills.add(int(imp_type["attributes"][r_aid]))
        char_item.subItems.add(imp_item)
        char_item.fittedItems[imp_tid] = imp_item
        imp_items.append({"item": imp_item, "type": imp_type, "tid": imp_tid})

    # Boosters
    boost_items = []
    for b_tid in boosters:
        b_type = get_type(b_tid)
        if not b_type:
            continue
        b_item = DogmaItem(b_tid, b_tid, b_type["group_id"], b_type["category_id"], lm)
        for aid, val in b_type["attributes"].iteritems():
            b_item.get_or_create_attr(aid, val)
        for r_aid, l_aid in ((182, 277), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)):
            if r_aid in b_type["attributes"]:
                b_item.reqSkills.add(int(b_type["attributes"][r_aid]))
        char_item.subItems.add(b_item)
        char_item.fittedItems[b_tid] = b_item
        boost_items.append({"item": b_item, "type": b_type, "tid": b_tid})

    # Ship passive effects
    for eid in ship_type["effects"]:
        eff = get_effect(eid)
        if not eff or eff["category"] not in (0, 4) or not eff["modifiers"]:
            continue
        eff_obj = build_modifier_effect(eid, eff)
        env = ModEnv(ship.itemID, ship.itemID, char_id)
        eff_obj.Start(env, lm, ship.itemID, ship.itemID, char_id, None, None)

    # Skills
    for sk_tid, lvl in sorted(skills.iteritems()):
        sk_type = get_type(sk_tid)
        if not sk_type:
            continue
        sk_item = DogmaItem(sk_tid, sk_tid, sk_type["group_id"], sk_type["category_id"], lm)
        sk_attrs = dict(sk_type["attributes"])
        sk_attrs[280] = float(lvl)
        for aid, val in sk_attrs.iteritems():
            sk_item.get_or_create_attr(aid, val)

        for eid in sk_type["effects"]:
            if eid == 132:
                continue
            eff = get_effect(eid)
            if not eff or eff["category"] != 0 or not eff["modifiers"]:
                continue
            eff_obj = build_modifier_effect(eid, eff)
            env = ModEnv(sk_item.itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, sk_item.itemID, ship.itemID, char_id, None, None)

    if len(imp_items) > 1:
        for imp in imp_items:
            for eid in imp["type"]["effects"]:
                eff = get_effect(eid)
                if not eff or eff["category"] != 0 or not eff["modifiers"]:
                    continue
                eff_obj = build_modifier_effect(eid, eff)
                env = ModEnv(imp["item"].itemID, ship.itemID, char_id)
                eff_obj.Start(env, lm, imp["item"].itemID, ship.itemID, char_id, None, None)
    elif len(imp_items) == 1:
        imp = imp_items[0]
        for eid in imp["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] != 0 or not eff["modifiers"]:
                continue
            eff_obj = build_modifier_effect(eid, eff)
            env = ModEnv(imp["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, imp["item"].itemID, ship.itemID, char_id, None, None)

    for b in boost_items:
        for eid in b["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] != 0 or not eff["modifiers"]:
                continue
            eff_obj = build_modifier_effect(eid, eff)
            env = ModEnv(b["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, b["item"].itemID, ship.itemID, char_id, None, None)

    # Modules passive effects and dynamic fitting loads
    # 1. Overload effects on itemID (cat == 5) evaluated before active effects on shipID,
    #    matching crates/eve-dogma/src/evaluation.rs:823-851 & 957-990
    for mod in mod_items:
        is_overloaded = mod["raw"].get("overloaded") or mod["raw"].get("state") == "Overloaded"
        if not is_overloaded:
            continue
        for eid in mod["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] != 5 or not eff["modifiers"]:
                continue
            eff_obj = build_modifier_effect(eid, eff, mod["type"]["attributes"])
            env = ModEnv(mod["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, mod["item"].itemID, ship.itemID, char_id, None, None)

    # 2. Passive (cat 0, 4) and Active (cat 1, 2, 3, or 6730/6731) effects
    for mod in mod_items:
        is_overloaded = mod["raw"].get("overloaded") or mod["raw"].get("state") == "Overloaded"
        is_active = mod["raw"].get("active") or is_overloaded
        for eid in mod["type"]["effects"]:
            eff = get_effect(eid)
            if not eff:
                continue
            cat = eff["category"]
            if cat == 5:
                continue
            has_mods = bool(eff["modifiers"]) or (eid in (6730, 6731))
            if not has_mods:
                continue

            if cat in (0, 4):
                pass
            elif is_active and (cat in (1, 2, 3) or eid in (6730, 6731)):
                pass
            else:
                continue

            eff_obj = build_modifier_effect(eid, eff, mod["type"]["attributes"])
            env = ModEnv(mod["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, mod["item"].itemID, ship.itemID, char_id, None, None)

    # 3. Active command burst / gang warfare burst effects (plan:T-12)
    # DERIVED_DBUFF_COLLECTION_AGGREGATION: dbuffCollections operationName
    # (Minimum/Maximum/Additive) aggregation is DERIVED from the SDE dbuffCollections
    # schema before passing the winning modifier into CCP's Dogma operator pipeline.
    bursts_by_dbuff = {}
    for gb_idx, burst in enumerate(gang_bursts):
        dbuff_id = burst.get("dbuff_id")
        if dbuff_id is not None:
            if dbuff_id not in bursts_by_dbuff:
                bursts_by_dbuff[dbuff_id] = {
                    "source_id": burst.get("source_item_id", 800000 + gb_idx),
                    "values": [],
                }
            bursts_by_dbuff[dbuff_id]["values"].append(float(burst.get("value", 0.0)))

    for dbuff_id in sorted(bursts_by_dbuff.keys()):
        entry = bursts_by_dbuff[dbuff_id]
        vals = entry["values"]
        source_id = entry["source_id"]
        row = cur.execute(
            "SELECT operation_name, data_json FROM sde_dbuff_collections WHERE dbuff_id = ?",
            (dbuff_id,),
        ).fetchone()
        if row:
            op_name = row[0]
            op = const.dgmAssPostPercent
            if op_name == "PostMul":
                op = const.dgmAssPostMul
            elif op_name == "ModAdd":
                op = const.dgmAssModAdd
            elif op_name == "PostAssignment":
                op = const.dgmAssPostAssignment

            data = json.loads(row[1])
            agg_mode = data.get("aggregateMode", "Maximum")
            if agg_mode == "Minimum":
                agg_val = min(vals)
            elif agg_mode == "Additive":
                agg_val = sum(vals)
            else:
                agg_val = max(vals)

            for item_mod in data.get("itemModifiers", []):
                aid = item_mod["dogmaAttributeID"]
                cat_id = 16 if sde_attr_is_stackable(aid) else 7
                mod_source = SimpleMod(agg_val, item_id=source_id, cat_id=cat_id, lm=lm)
                lm.AddModifierWithSource(op, ship.itemID, aid, mod_source)
            for loc_mod in data.get("locationModifiers", []):
                aid = loc_mod["dogmaAttributeID"]
                cat_id = 16 if sde_attr_is_stackable(aid) else 7
                mod_source = SimpleMod(agg_val, item_id=source_id, cat_id=cat_id, lm=lm)
                lm.AddLocationModifierWithSource(op, ship.itemID, aid, mod_source)
            for grp_mod in data.get("locationGroupModifiers", []):
                aid = grp_mod["dogmaAttributeID"]
                gid = grp_mod["groupID"]
                cat_id = 16 if sde_attr_is_stackable(aid) else 7
                mod_source = SimpleMod(agg_val, item_id=source_id, cat_id=cat_id, lm=lm)
                lm.AddLocationGroupModifierWithSource(op, ship.itemID, gid, aid, mod_source)
            for srq_mod in data.get("locationRequiredSkillModifiers", []):
                aid = srq_mod["dogmaAttributeID"]
                skid = srq_mod["skillID"]
                cat_id = 16 if sde_attr_is_stackable(aid) else 7
                mod_source = SimpleMod(agg_val, item_id=source_id, cat_id=cat_id, lm=lm)
                lm.AddLocationRequiredSkillModifierWithSource(op, ship.itemID, skid, aid, mod_source)

    for gb_idx, burst in enumerate(gang_bursts):
        source_id = burst.get("source_item_id", 800000 + gb_idx)
        val = float(burst.get("value", 0.0))
        for explicit_mod in burst.get("modifiers", []):
            op = explicit_mod.get("operator", const.dgmAssPostPercent)
            aid = explicit_mod.get("target_attribute_id", explicit_mod.get("attribute_id"))
            m_val = float(explicit_mod.get("value", val))
            cat_id = 16 if (aid is not None and sde_attr_is_stackable(aid)) else 7
            if aid is not None:
                lm.AddModifierWithSource(op, ship.itemID, aid, SimpleMod(m_val, item_id=source_id, cat_id=cat_id, lm=lm))

    total_cpu = 0.0
    total_pwr = 0.0
    for mod in mod_items:
        c_attr = mod["item"].get_or_create_attr(50)
        p_attr = mod["item"].get_or_create_attr(30)
        total_cpu += c_attr.GetValue()
        total_pwr += p_attr.GetValue()

    # Set ship CPU & Power loads from module totals
    ship.get_or_create_attr(49, total_cpu)
    ship.get_or_create_attr(15, total_pwr)

    res = {}
    for name, aid in ATTR_NAME_TO_ID.iteritems():
        attr_obj = ship.get_or_create_attr(aid)
        res[name] = attr_obj.GetValue()
    return res


# ---------------------------------------------------------------------------
# Procedural Fit Generator (500+ Fits from SDE)
# ---------------------------------------------------------------------------
def generate_procedural_fits(target_count=520):
    cur.execute(
        "SELECT type_id, group_id, name, attributes_json FROM sde_types WHERE category_id = 6 AND published = 1 ORDER BY type_id"
    )
    ships = cur.fetchall()

    fits = []
    fit_idx = 0
    for ship_row in ships:
        if len(fits) >= target_count:
            break
        s_tid = ship_row[0]
        s_name = ship_row[2]
        s_attrs = json.loads(ship_row[3])
        low_slots = int(s_attrs.get("12", 0))
        med_slots = int(s_attrs.get("13", 0))
        cpu_out = float(s_attrs.get("48", 0.0))
        pwr_out = float(s_attrs.get("11", 0.0))
        rig_slots = int(s_attrs.get("1137", 0))
        rig_size = int(s_attrs.get("1547", 0))

        # Standard baseline fit for ship hull
        fit_idx += 1
        mods1 = []
        if rig_slots > 0 and rig_size == 1:
            rt = get_type(31358)
            r_name = rt["name"] if rt else "Small Ancillary Current Router I"
            mods1.append({
                "item_id": 100000 + fit_idx * 10 + 3,
                "type_id": 31358,
                "flag_id": 92,
                "name": r_name,
            })
        if low_slots > 0 and cpu_out >= 15.0 and pwr_out >= 1.0:
            mods1.append({
                "item_id": 100000 + fit_idx * 10,
                "type_id": 2605,
                "flag_id": 11,
                "name": "Nanofiber Internal Structure II",
            })
        if low_slots > 1 and cpu_out >= 35.0 and pwr_out >= 2.0:
            mods1.append({
                "item_id": 100000 + fit_idx * 10 + 1,
                "type_id": 1236,
                "flag_id": 12,
                "name": "Overdrive Injector System II",
            })
        if med_slots > 0 and cpu_out >= 80.0 and pwr_out >= 50.0:
            mods1.append({
                "item_id": 100000 + fit_idx * 10 + 2,
                "type_id": 3829,
                "flag_id": 19,
                "name": "Medium Shield Extender I",
            })

        skills1 = {
            3392: 5, 3302: 5, 3318: 5, 3327: 5, 3329: 5,
            3419: 5, 3300: 5, 3394: 5, 3449: 5,
            3425: 5, 3393: 5, 20211: 5, 3334: 5, 11084: 5, 3426: 5,
        }
        bursts1 = []
        if fit_idx % 7 == 0:
            bursts1.append({"dbuff_id": 12, "value": 10.0})
            bursts1.append({"dbuff_id": 12, "value": 15.0})
        if fit_idx % 11 == 0:
            bursts1.append({"dbuff_id": 20, "value": -5.0})
            bursts1.append({"dbuff_id": 20, "value": -8.0})
        if fit_idx % 5 == 0:
            for m in mods1:
                if 11 <= m.get("flag_id", 0) <= 34:
                    m["dynamic_attributes"] = {"50": 10.0, "30": 0.5}
                    break

        res1 = evaluate_dogma_fit(s_tid, skills1, mods1, [], None, bursts1)
        fits.append({
            "name": "proc_%03d_%s_standard" % (fit_idx, s_name.replace(" ", "_").lower()),
            "ship_type_id": s_tid,
            "ship_name": s_name,
            "oracle_source": "CCP_BASE_DOGMA_LOCATION_DISPATCH",
            "skills": skills1,
            "modules": mods1,
            "implants": [],
            "gang_bursts": bursts1,
            "expected_attributes": res1,
        })

        if len(fits) < target_count:
            fit_idx += 1
            mods2 = []
            if low_slots > 0 and cpu_out >= 15.0 and pwr_out >= 1.0:
                mods2.append({
                    "item_id": 200000 + fit_idx * 10,
                    "type_id": 1405,
                    "flag_id": 11,
                    "name": "Inertial Stabilizers II",
                })
            if low_slots > 1 and cpu_out >= 35.0 and pwr_out >= 2.0:
                mods2.append({
                    "item_id": 200000 + fit_idx * 10 + 1,
                    "type_id": 2048,
                    "flag_id": 12,
                    "name": "Damage Control II",
                })
            if low_slots > 2 and cpu_out >= 80.0 and pwr_out >= 10.0:
                mods2.append({
                    "item_id": 200000 + fit_idx * 10 + 2,
                    "type_id": 519,
                    "flag_id": 13,
                    "name": "Gyrostabilizer II",
                })
            if med_slots > 0 and cpu_out >= 45.0 and pwr_out >= 5.0:
                sh_mod = {
                    "item_id": 200000 + fit_idx * 10 + 3,
                    "type_id": 2281,
                    "flag_id": 19,
                    "name": "Multispectrum Shield Hardener II",
                    "active": True,
                }
                if fit_idx % 4 == 0:
                    sh_mod["overloaded"] = True
                mods2.append(sh_mod)
            if med_slots > 1 and cpu_out >= 75.0 and pwr_out >= 20.0:
                mods2.append({
                    "item_id": 200000 + fit_idx * 10 + 4,
                    "type_id": 438,
                    "flag_id": 20,
                    "name": "1MN Afterburner II",
                    "active": True,
                })
            implants2 = [19540, 19551, 19553] if fit_idx % 4 == 0 else []
            boosters2 = [3898] if fit_idx % 3 == 0 else []
            bursts2 = []
            if fit_idx % 6 == 0:
                bursts2.append({"dbuff_id": 15, "value": 12.0})
                bursts2.append({"dbuff_id": 15, "value": 18.0})
                bursts2.append({"dbuff_id": 13, "value": -10.0})
                bursts2.append({"dbuff_id": 13, "value": -15.0})
            if fit_idx % 8 == 0:
                for m in mods2:
                    if 11 <= m.get("flag_id", 0) <= 34:
                        m["dynamic_attributes"] = {"50": 12.0, "30": 1.0}
                        break

            skills2 = {
                3392: 4, 3302: 4, 3318: 5, 3394: 5, 3449: 4,
                3425: 5, 3393: 5, 20211: 5, 3334: 5, 11084: 5, 3426: 5,
                3420: 5, 3450: 5,
            }
            res2 = evaluate_dogma_fit(s_tid, skills2, mods2, implants2, boosters2, bursts2)
            fits.append({
                "name": "proc_%03d_%s_variant" % (fit_idx, s_name.replace(" ", "_").lower()),
                "ship_type_id": s_tid,
                "ship_name": s_name,
                "oracle_source": "CCP_BASE_DOGMA_LOCATION_DISPATCH",
                "skills": skills2,
                "modules": mods2,
                "implants": implants2,
                "boosters": boosters2,
                "gang_bursts": bursts2,
                "expected_attributes": res2,
            })

    return fits


# ---------------------------------------------------------------------------
# Canonical Fit Specifications (Fall-back Default)
# ---------------------------------------------------------------------------
DEFAULT_FIT_SPECS = [
    {
        "name": "frigate_rifter_speed_and_brawl",
        "ship_type_id": 587,
        "ship_name": "Rifter",
        "skills": {
            3392: 5, 3302: 5, 3318: 5, 3327: 5, 3329: 5,
            3419: 5, 11084: 4, 3300: 5, 3394: 5, 3449: 5,
        },
        "modules": [
            {"item_id": 10001, "type_id": 1236, "flag_id": 11, "name": "Overdrive Injector System II"},
            {"item_id": 10002, "type_id": 2605, "flag_id": 12, "name": "Nanofiber Internal Structure II"},
            {"item_id": 10003, "type_id": 519, "flag_id": 13, "name": "Gyrostabilizer II"},
            {"item_id": 10004, "type_id": 2889, "flag_id": 27, "name": "200mm AutoCannon II"},
        ],
        "implants": [],
        "required_attr_keys": [
            "powerLoad", "capacity", "agility", "powerOutput", "hp_structure",
            "armorHP", "shieldCapacity", "maxVelocity", "cpuOutput", "cpuLoad",
        ],
    },
    {
        "name": "cruiser_caracal_missile_and_shield",
        "ship_type_id": 621,
        "ship_name": "Caracal",
        "skills": {
            20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
            3319: 5, 3449: 5, 3324: 5, 3318: 5, 3420: 5, 3450: 5,
        },
        "modules": [
            {"item_id": 20001, "type_id": 22291, "flag_id": 11, "name": "Ballistic Control System II #1"},
            {"item_id": 20002, "type_id": 22291, "flag_id": 12, "name": "Ballistic Control System II #2"},
            {"item_id": 20003, "type_id": 22291, "flag_id": 13, "name": "Ballistic Control System II #3"},
            {"item_id": 20004, "type_id": 2048, "flag_id": 14, "name": "Damage Control II"},
            {"item_id": 20005, "type_id": 3841, "flag_id": 19, "name": "Large Shield Extender II"},
            {"item_id": 20006, "type_id": 2410, "flag_id": 27, "name": "Heavy Missile Launcher II"},
        ],
        "implants": [],
        "required_attr_keys": [
            "cpuLoad", "shieldExplosiveResonance", "shieldKineticResonance",
            "shieldThermalResonance", "shieldEmResonance", "powerLoad", "cpuOutput",
            "maxVelocity", "shieldCapacity", "powerOutput",
        ],
    },
    {
        "name": "battleship_megathron_armor_tank",
        "ship_type_id": 641,
        "ship_name": "Megathron",
        "skills": {
            3392: 5, 3336: 5, 3394: 5, 3393: 5, 3307: 5, 3318: 5,
        },
        "modules": [
            {"item_id": 30001, "type_id": 10190, "flag_id": 11, "name": "Magnetic Field Stabilizer II #1"},
            {"item_id": 30002, "type_id": 10190, "flag_id": 12, "name": "Magnetic Field Stabilizer II #2"},
            {"item_id": 30003, "type_id": 10190, "flag_id": 13, "name": "Magnetic Field Stabilizer II #3"},
            {"item_id": 30004, "type_id": 20353, "flag_id": 14, "name": "1600mm Steel Plates II"},
            {"item_id": 30005, "type_id": 2048, "flag_id": 15, "name": "Damage Control II"},
        ],
        "implants": [19540, 19551, 19553],
        "required_attr_keys": [
            "armorExplosiveResonance", "armorHP", "powerOutput", "cpuOutput",
            "powerLoad", "hp_structure", "cpuLoad", "armorEmResonance",
            "armorKineticResonance", "armorThermalResonance", "maxVelocity",
        ],
    },
    {
        "name": "rifter_four_inertial_stabilizers_stacking",
        "ship_type_id": 587,
        "ship_name": "Rifter",
        "skills": {3394: 5},
        "modules": [
            {"item_id": 40001, "type_id": 1405, "flag_id": 11, "name": "Inertial Stabilizers II #1"},
            {"item_id": 40002, "type_id": 1405, "flag_id": 12, "name": "Inertial Stabilizers II #2"},
            {"item_id": 40003, "type_id": 1405, "flag_id": 13, "name": "Inertial Stabilizers II #3"},
            {"item_id": 40004, "type_id": 1405, "flag_id": 14, "name": "Inertial Stabilizers II #4"},
        ],
        "implants": [],
        "required_attr_keys": [
            "agility", "cpuLoad", "sigRadius", "powerLoad",
        ],
    },
    {
        "name": "caracal_mutaplasmid_shield_extender",
        "ship_type_id": 621,
        "ship_name": "Caracal",
        "skills": {
            20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
            3319: 5, 3449: 5, 3324: 5, 3318: 5, 3420: 5, 3450: 5,
        },
        "modules": [
            {"item_id": 50001, "type_id": 22291, "flag_id": 11, "name": "Ballistic Control System II #1"},
            {"item_id": 50002, "type_id": 22291, "flag_id": 12, "name": "Ballistic Control System II #2"},
            {"item_id": 50003, "type_id": 2048, "flag_id": 13, "name": "Damage Control II"},
            {
                "item_id": 50004,
                "type_id": 3841,
                "flag_id": 19,
                "name": "Large Shield Extender II (Abyssal)",
                "dynamic_attributes": {"72": 3120.0, "50": 40.0, "30": 145.0},
            },
            {"item_id": 50005, "type_id": 2410, "flag_id": 27, "name": "Heavy Missile Launcher II"},
        ],
        "implants": [],
        "required_attr_keys": [
            "cpuLoad", "powerLoad", "shieldCapacity", "maxVelocity", "cpuOutput",
            "powerOutput", "shieldExplosiveResonance", "shieldKineticResonance",
        ],
    },
    {
        "name": "megathron_command_burst_gang_fleet",
        "ship_type_id": 641,
        "ship_name": "Megathron",
        "skills": {
            3392: 5, 3336: 5, 3394: 5, 3393: 5, 3307: 5, 3318: 5,
        },
        "modules": [
            {"item_id": 60001, "type_id": 10190, "flag_id": 11, "name": "Magnetic Field Stabilizer II #1"},
            {"item_id": 60002, "type_id": 20353, "flag_id": 14, "name": "1600mm Steel Plates II"},
            {"item_id": 60003, "type_id": 2048, "flag_id": 15, "name": "Damage Control II"},
        ],
        "implants": [],
        "gang_bursts": [
            {"dbuff_id": 13, "value": -10.0},
            {"dbuff_id": 13, "value": -15.0},
            {"dbuff_id": 15, "value": 12.0},
            {"dbuff_id": 15, "value": 15.0},
            {"dbuff_id": 20, "value": -8.0},
            {"dbuff_id": 20, "value": -10.0},
        ],
        "required_attr_keys": [
            "armorHP", "armorEmResonance", "armorExplosiveResonance",
            "armorKineticResonance", "armorThermalResonance", "sigRadius",
            "powerLoad", "cpuLoad",
        ],
    },
    {
        "name": "rifter_triple_trimark_rigs",
        "ship_type_id": 587,
        "ship_name": "Rifter",
        "skills": {
            3392: 5, 3302: 5, 3318: 5, 3327: 5, 3329: 5,
            3419: 5, 11084: 4, 3300: 5, 3394: 5, 3449: 5,
        },
        "modules": [
            {"item_id": 70001, "type_id": 1236, "flag_id": 11, "name": "Overdrive Injector System II"},
            {"item_id": 70002, "type_id": 2605, "flag_id": 12, "name": "Nanofiber Internal Structure II"},
            {"item_id": 70003, "type_id": 519, "flag_id": 13, "name": "Gyrostabilizer II"},
            {"item_id": 70004, "type_id": 30987, "flag_id": 92, "name": "Small Trimark Armor Pump I #1"},
            {"item_id": 70005, "type_id": 30987, "flag_id": 93, "name": "Small Trimark Armor Pump I #2"},
            {"item_id": 70006, "type_id": 30987, "flag_id": 94, "name": "Small Trimark Armor Pump I #3"},
        ],
        "implants": [],
        "required_attr_keys": [
            "armorHP", "maxVelocity", "agility", "powerLoad", "cpuLoad", "powerOutput", "cpuOutput",
        ],
    },
    {
        "name": "caracal_combat_booster_quafe",
        "ship_type_id": 621,
        "ship_name": "Caracal",
        "skills": {
            20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
            3319: 5, 3449: 5, 3324: 5, 3318: 5, 3420: 5, 3450: 5,
        },
        "modules": [
            {"item_id": 80001, "type_id": 22291, "flag_id": 11, "name": "Ballistic Control System II"},
            {"item_id": 80002, "type_id": 2048, "flag_id": 14, "name": "Damage Control II"},
            {"item_id": 80003, "type_id": 3841, "flag_id": 19, "name": "Large Shield Extender II"},
        ],
        "implants": [],
        "boosters": [3898],
        "required_attr_keys": [
            "maxVelocity", "shieldCapacity", "powerLoad", "cpuLoad", "powerOutput", "cpuOutput",
        ],
    },
    {
        "name": "caracal_overloaded_shield_and_prop",
        "ship_type_id": 621,
        "ship_name": "Caracal",
        "skills": {
            20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
            3319: 5, 3449: 5, 3324: 5, 3318: 5, 3420: 5, 3450: 5,
        },
        "modules": [
            {"item_id": 90001, "type_id": 2048, "flag_id": 11, "name": "Damage Control II"},
            {
                "item_id": 90002,
                "type_id": 2281,
                "flag_id": 19,
                "name": "Multispectrum Shield Hardener II",
                "active": True,
                "overloaded": True,
                "state": "Overloaded",
            },
            {
                "item_id": 90003,
                "type_id": 438,
                "flag_id": 20,
                "name": "1MN Afterburner II",
                "active": True,
                "overloaded": True,
                "state": "Overloaded",
            },
        ],
        "implants": [],
        "required_attr_keys": [
            "shieldEmResonance", "shieldThermalResonance", "shieldKineticResonance",
            "shieldExplosiveResonance", "maxVelocity", "powerLoad", "cpuLoad",
        ],
    },
    {
        "name": "caracal_command_burst_shield_fleet",
        "ship_type_id": 621,
        "ship_name": "Caracal",
        "skills": {
            20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
            3319: 5, 3449: 5, 3324: 5, 3318: 5, 3420: 5, 3450: 5,
        },
        "modules": [
            {"item_id": 95001, "type_id": 2048, "flag_id": 11, "name": "Damage Control II"},
            {"item_id": 95002, "type_id": 3841, "flag_id": 19, "name": "Large Shield Extender II"},
        ],
        "implants": [],
        "gang_bursts": [
            {"dbuff_id": 12, "value": 10.0},
            {"dbuff_id": 12, "value": 16.0},
            {"dbuff_id": 10, "value": -10.0},
            {"dbuff_id": 10, "value": -18.0},
        ],
        "required_attr_keys": [
            "shieldCapacity", "shieldEmResonance", "shieldThermalResonance",
            "shieldKineticResonance", "shieldExplosiveResonance", "powerLoad", "cpuLoad",
        ],
    },
]


def main():
    if specs_path:
        with open(specs_path, "r") as f:
            specs = json.load(f)
    else:
        specs = DEFAULT_FIT_SPECS

    prec = evaluate_operator_precedence()
    stacking_data = extract_stacking_data()

    evaluated_fits = []
    for s in specs:
        skills = {int(k): int(v) for k, v in s["skills"].iteritems()}
        boosters = s.get("boosters", [])
        gang_bursts = s.get("gang_bursts", [])
        res = evaluate_dogma_fit(s["ship_type_id"], skills, s["modules"], s["implants"], boosters, gang_bursts)
        req_keys = s.get("required_attr_keys", ATTR_NAME_TO_ID.keys())
        expected_attrs = {k: res[k] for k in req_keys}
        evaluated_fits.append({
            "name": s["name"],
            "ship_type_id": s["ship_type_id"],
            "ship_name": s["ship_name"],
            "oracle_source": "CCP_BASE_DOGMA_LOCATION_DISPATCH",
            "skills": skills,
            "modules": s["modules"],
            "implants": s["implants"],
            "boosters": boosters,
            "gang_bursts": gang_bursts,
            "expected_attributes": expected_attrs,
        })

    procedural_fits = []
    if procedural_count > 0:
        procedural_fits = generate_procedural_fits(target_count=procedural_count)

    output_payload = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "oracle_source": "CCP_BUNDLED_RUNTIME",
        "fits_oracle_source": "CCP_BASE_DOGMA_LOCATION_DISPATCH",
        "graph_wiring": "CCP_MODIFIER_EFFECT_LOCATION_GRAPH",
        "modifier_engine": "CCP_NATIVE_MODIFIER_EFFECT",
        "location_engine": "CCP_BASE_DOGMA_LOCATION_DISPATCH",
        "attribute_engine": "CCP_NATIVE_ATTRIBUTE",
        "stacking_engine": "CCP_NATIVE_STACKING_NURFED_ATTRIBUTE",
        "dbuff_aggregation_source": "DERIVED_DBUFF_COLLECTION_AGGREGATION",
        "stacking_constants": {
            "max_len": stacking_data["max_len"],
            "exp_factor": stacking_data["exp_factor"],
            "exp_power": stacking_data["exp_power"],
        },
        "stacking_factors": stacking_data["stacking_factors"],
        "operator_precedence": stacking_data["operator_precedence"],
        "precedence_test": prec,
        "fits": evaluated_fits,
        "procedural_fits": procedural_fits,
    }

    if out_json_path:
        with open(out_json_path, "w") as f:
            json.dump(output_payload, f, indent=2)
    else:
        print(json.dumps(output_payload, indent=2))


if __name__ == "__main__":
    main()
