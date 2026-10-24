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
Scales across 4 canonical fits and 500+ procedural ship/module/implant fits.
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


def _load_ext(name):
    mod = __import__(name)
    sys.modules[name] = mod
    return mod


mock_blue.LoadExtension = _load_ext
mock_blue.pyos.LoadExtension = _load_ext
sys.modules["blue"] = mock_blue

uthread_mod = types.ModuleType("uthread")
uthread_mod.new = lambda fn, *a, **k: None
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

import dogma.const as const  # noqa: E402
import dogma.attributes.attribute as attr  # noqa: E402
import dogma.attributes.stackingNurfedAttribute as sna  # noqa: E402
import dogma.effects.modifiers as mods  # noqa: E402
import dogma.effects.modifiereffect as me  # noqa: E402
import dogma.mocks.mockDogmaLocation as mdl  # noqa: E402



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


class DogmaItem(object):
    def __init__(self, item_id, type_id, group_id, category_id, lm):
        self.itemID = item_id
        self.invItem = MockInvItem(item_id, type_id, group_id, category_id)
        self.typeID = type_id
        self.groupID = group_id
        self.categoryID = category_id
        self.attributes = {}
        self.subItems = set()
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

    def get_or_create_attr(self, aid, base_val=None):
        if aid in self.attributes:
            return self.attributes[aid]
        def_val, high_is_good, stackable = get_attr_meta(aid)
        if base_val is None:
            base_val = def_val
        sa = StaticAttr(aid, def_val, high_is_good, stackable)
        if stackable:
            a = attr.Attribute(aid, base_val, self, self.lm, sa)
        else:
            a = sna.StackingNurfedAttribute(aid, base_val, self, self.lm, sa)
        self.attributes[aid] = a
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


class EvoMuDogmaLocation(mdl.MockDogmaLocation):
    """Subclass of CCP's MockDogmaLocation wired to handle Dogma modifier dispatch."""

    def __init__(self):
        mdl.MockDogmaLocation.__init__(self)
        self.AddLocationRequiredSkillModifier = self.AddLocationSkillModifier
        self.AddOwnerRequiredSkillModifier = self.AddOwnerSkillModifier

    def AddModifier(self, operation, toItemID, toAttribID, fromItemID, fromAttribID):
        fromItem = self.dogmaItems.get(fromItemID)
        if not fromItem:
            return
        fromAttr = fromItem.get_or_create_attr(fromAttribID)
        toItem = self.dogmaItems.get(toItemID)
        if not toItem:
            return
        toAttr = toItem.get_or_create_attr(toAttribID)
        fromAttr.AddOutgoingModifier(operation, toAttr)
        toAttr.AddIncomingModifier(operation, fromAttr)

    def AddLocationModifier(self, op, toLocationID, toAttribID, fromItemID, fromAttribID):
        locItem = self.dogmaItems.get(toLocationID)
        if not locItem:
            return
        fromItem = self.dogmaItems.get(fromItemID)
        if not fromItem:
            return
        fromAttr = fromItem.get_or_create_attr(fromAttribID)
        locItem.locationMods[toAttribID].add((op, fromAttr))
        for item in locItem.subItems:
            toAttr = item.get_or_create_attr(toAttribID)
            fromAttr.AddOutgoingModifier(op, toAttr)
            toAttr.AddIncomingModifier(op, fromAttr)

    def AddLocationGroupModifier(self, op, toLocationID, groupID, toAttribID, fromItemID, fromAttribID):
        locItem = self.dogmaItems.get(toLocationID)
        if not locItem:
            return
        fromItem = self.dogmaItems.get(fromItemID)
        if not fromItem:
            return
        fromAttr = fromItem.get_or_create_attr(fromAttribID)
        locItem.locationGroupMods[groupID][toAttribID].add((op, fromAttr))
        for item in locItem.subItems:
            if item.groupID == groupID:
                toAttr = item.get_or_create_attr(toAttribID)
                fromAttr.AddOutgoingModifier(op, toAttr)
                toAttr.AddIncomingModifier(op, fromAttr)

    def AddLocationSkillModifier(self, op, toLocationID, skillTypeID, toAttribID, fromItemID, fromAttribID):
        locItem = self.dogmaItems.get(toLocationID)
        if not locItem:
            return
        fromItem = self.dogmaItems.get(fromItemID)
        if not fromItem:
            return
        fromAttr = fromItem.get_or_create_attr(fromAttribID)
        locItem.locationReqSkillMods[skillTypeID][toAttribID].add((op, fromAttr))
        for item in locItem.subItems:
            if skillTypeID in item.reqSkills:
                toAttr = item.get_or_create_attr(toAttribID)
                fromAttr.AddOutgoingModifier(op, toAttr)
                toAttr.AddIncomingModifier(op, fromAttr)

    def AddOwnerSkillModifier(self, op, ownerID, skillTypeID, toAttribID, fromItemID, fromAttribID):
        ownerItem = self.dogmaItems.get(ownerID)
        if not ownerItem:
            return
        fromItem = self.dogmaItems.get(fromItemID)
        if not fromItem:
            return
        fromAttr = fromItem.get_or_create_attr(fromAttribID)
        ownerItem.ownerReqSkillMods[skillTypeID][toAttribID].add((op, fromAttr))
        for item in ownerItem.subItems:
            if skillTypeID in item.reqSkills:
                toAttr = item.get_or_create_attr(toAttribID)
                fromAttr.AddOutgoingModifier(op, toAttr)
                toAttr.AddIncomingModifier(op, fromAttr)


def build_modifier_effect(eff_id, eff_spec):
    modifier_objs = []
    for spec in eff_spec["modifiers"]:
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
# 10-Operator Precedence Verification via CCP's Attribute Class
# ---------------------------------------------------------------------------
def evaluate_operator_precedence():
    """Evaluates CCP's authentic 10-operator precedence sequence via Attribute."""
    lm = EvoMuDogmaLocation()
    item = DogmaItem(999, 587, 25, 6, lm)
    sa = StaticAttr(37, 100.0, True, True)
    target_attr = attr.Attribute(37, 100.0, item, lm, sa)

    class SimpleMod(object):
        def __init__(self, val, item_id, cat_id=16):
            self.val = val
            self.dogmaItem = DogmaItem(item_id, 1, 1, cat_id, lm)
            self.item = self.dogmaItem

        def GetValue(self):
            return self.val

    target_attr.AddIncomingModifier(2, SimpleMod(20.0, item_id=1))
    target_attr.AddIncomingModifier(3, SimpleMod(10.0, item_id=2))
    target_attr.AddIncomingModifier(0, SimpleMod(2.0, item_id=3))
    target_attr.AddIncomingModifier(1, SimpleMod(2.0, item_id=4))
    target_attr.AddIncomingModifier(-1, SimpleMod(50.0, item_id=5))
    target_attr.AddIncomingModifier(4, SimpleMod(1.5, item_id=6))
    target_attr.AddIncomingModifier(5, SimpleMod(1.25, item_id=7))
    target_attr.AddIncomingModifier(6, SimpleMod(10.0, item_id=8))
    target_attr.AddIncomingModifier(8, SimpleMod(10.0, item_id=9))

    val_before_post_assign = target_attr.GetValue()
    target_attr.AddIncomingModifier(7, SimpleMod(250.0, item_id=10))
    val_after_post_assign = target_attr.GetValue()

    return {
        "val_before_post_assign": val_before_post_assign,
        "val_after_post_assign": val_after_post_assign,
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
def evaluate_dogma_fit(ship_type_id, skills, modules, implants, boosters=None):
    if boosters is None:
        boosters = []
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
        for req_aid in (182, 183, 184, 1285, 1289, 1290):
            if req_aid in mt["attributes"]:
                m_item.reqSkills.add(int(mt["attributes"][req_aid]))
        ship.subItems.add(m_item)
        mod_items.append({"item": m_item, "type": mt, "raw": m})

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

    # Implants
    imp_items = []
    for imp_tid in implants:
        imp_type = get_type(imp_tid)
        if not imp_type:
            continue
        imp_item = DogmaItem(imp_tid, imp_tid, imp_type["group_id"], imp_type["category_id"], lm)
        for aid, val in imp_type["attributes"].iteritems():
            imp_item.get_or_create_attr(aid, val)
        char_item.subItems.add(imp_item)
        imp_items.append({"item": imp_item, "type": imp_type, "tid": imp_tid})

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

    # Boosters
    boost_items = []
    for b_tid in boosters:
        b_type = get_type(b_tid)
        if not b_type:
            continue
        b_item = DogmaItem(b_tid, b_tid, b_type["group_id"], b_type["category_id"], lm)
        for aid, val in b_type["attributes"].iteritems():
            if aid in (1083, 1084, 1085, 1086, 1087):
                continue
            b_item.get_or_create_attr(aid, val)
        char_item.subItems.add(b_item)
        boost_items.append({"item": b_item, "type": b_type, "tid": b_tid})

    for b in boost_items:
        for eid in b["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] != 0 or not eff["modifiers"]:
                continue
            eff_obj = build_modifier_effect(eid, eff)
            env = ModEnv(b["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, b["item"].itemID, ship.itemID, char_id, None, None)

    # Modules passive effects and dynamic fitting loads
    for mod in mod_items:
        for eid in mod["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or not eff["modifiers"]:
                continue
            is_active = mod["raw"].get("active")
            is_overloaded = mod["raw"].get("overloaded")
            cat = eff["category"]
            if cat in (0, 4):
                pass
            elif is_active or is_overloaded:
                if cat in (1, 2, 3):
                    pass
                elif cat == 5 and is_overloaded:
                    pass
                else:
                    continue
            else:
                continue

            eff_obj = build_modifier_effect(eid, eff)
            env = ModEnv(mod["item"].itemID, ship.itemID, char_id)
            eff_obj.Start(env, lm, mod["item"].itemID, ship.itemID, char_id, None, None)

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
            mods1.append({
                "item_id": 100000 + fit_idx * 10 + 3,
                "type_id": 31358,
                "flag_id": 92,
                "name": "Small Auxiliary Nano Pump I",
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
        res1 = evaluate_dogma_fit(s_tid, skills1, mods1, [])
        fits.append({
            "name": "proc_%03d_%s_standard" % (fit_idx, s_name.replace(" ", "_").lower()),
            "ship_type_id": s_tid,
            "ship_name": s_name,
            "oracle_source": "CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH",
            "skills": skills1,
            "modules": mods1,
            "implants": [],
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
            implants2 = [19540, 19551, 19553] if fit_idx % 4 == 0 else []
            skills2 = {
                3392: 4, 3302: 4, 3318: 5, 3394: 5, 3449: 4,
                3425: 5, 3393: 5, 20211: 5, 3334: 5, 11084: 5, 3426: 5,
            }
            res2 = evaluate_dogma_fit(s_tid, skills2, mods2, implants2)
            fits.append({
                "name": "proc_%03d_%s_variant" % (fit_idx, s_name.replace(" ", "_").lower()),
                "ship_type_id": s_tid,
                "ship_name": s_name,
                "oracle_source": "CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH",
                "skills": skills2,
                "modules": mods2,
                "implants": implants2,
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
            3319: 5, 3449: 5, 3324: 5, 3318: 5,
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
        res = evaluate_dogma_fit(s["ship_type_id"], skills, s["modules"], s["implants"])
        req_keys = s.get("required_attr_keys", ATTR_NAME_TO_ID.keys())
        expected_attrs = {k: res[k] for k in req_keys}
        evaluated_fits.append({
            "name": s["name"],
            "ship_type_id": s["ship_type_id"],
            "ship_name": s["ship_name"],
            "oracle_source": "CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH",
            "skills": skills,
            "modules": s["modules"],
            "implants": s["implants"],
            "expected_attributes": expected_attrs,
        })

    procedural_fits = []
    if procedural_count > 0:
        procedural_fits = generate_procedural_fits(target_count=procedural_count)

    output_payload = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "oracle_source": "CCP_BUNDLED_RUNTIME",
        "fits_oracle_source": "CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH",
        "graph_wiring": "CCP_MODIFIER_EFFECT_LOCATION_GRAPH",
        "modifier_engine": "CCP_NATIVE_MODIFIER_EFFECT",
        "location_engine": "CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH",
        "attribute_engine": "CCP_NATIVE_ATTRIBUTE",
        "stacking_engine": "CCP_NATIVE_STACKING_NURFED_ATTRIBUTE",
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
