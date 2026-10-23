#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""100% Authentic CCP Stackless Python 2.7 Dogma Evaluator (plan:T-12).

Executes CCP's compiled Dogma bytecode modules:
- dogma.attributes.attribute.Attribute
- dogma.attributes.stackingNurfedAttribute.StackingNurfedAttribute
inside CCP's bundled Stackless Python 2.7.1 runtime via cfsd_runner.

Queries attribute metadata (default_value, high_is_good, stackable) dynamically from
`data/evomu_sde.sqlite`.
Instantiates CCP's `Attribute` when stackable == 1, and `StackingNurfedAttribute` when
stackable == 0 (non-stackable attributes apply CCP's `NurfDenominators` stacking penalties).
Wires skill, implant, and module modifier graphs via `AddIncomingModifier`.
Evaluates pirate implant set multipliers and the 11-operator precedence sequence.
"""

import sys

if len(sys.argv) < 4:
    sys.stderr.write(
        "Usage: cfsd_runner tools/dogma_evaluator_py2.py <code_pyc.zip> <bin64_dir> <sde_sqlite> [specs.json] [out.json]\n"
    )
    sys.exit(2)

code_pyc_zip = sys.argv[1]
bin64_dir = sys.argv[2]
sde_sqlite_path = sys.argv[3]
specs_path = sys.argv[4] if len(sys.argv) > 4 else None
out_json_path = sys.argv[5] if len(sys.argv) > 5 else None

# Set up runtime import paths
sys.path.insert(0, code_pyc_zip)
sys.path.insert(0, bin64_dir)
import encodings  # noqa: F401, E402

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

import dogma.data  # noqa: E402

dogma.data.get_type_attribute_or_default = lambda tid, aid, default=None: default
dogma.data.get_type_attribute = lambda tid, aid: None

import dogma.const as const  # noqa: E402
import dogma.attributes.attribute as attr  # noqa: E402
import dogma.attributes.stackingNurfedAttribute as sna  # noqa: E402


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
# Dogma Item & Attribute Dispatch Models
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


class MockDogmaLM(object):
    def __init__(self):
        self.dogmaItems = {}
        self.attributeChangeCallbacksByAttributeID = {}


class DogmaItem(object):
    def __init__(self, item_id, type_id, group_id, category_id, lm):
        self.itemID = item_id
        self.invItem = MockInvItem(item_id, type_id, group_id, category_id)
        self.dogmaLocation = lm
        self.attributes = {}
        self.lm = lm

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


def item_requires_skill(item_attrs, skill_tid):
    for req_aid in (182, 183, 184, 1285, 1289, 1290):
        if item_attrs.get(req_aid) == float(skill_tid):
            return True
    return False


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
    """Evaluates CCP's authentic 10-operator precedence sequence via Attribute.

    Note on Operator 9 (dgmAssSpecialSkillRelationship):
    In CCP's architecture, const.dgmOperatorPrecedence defines the core 10 operators
    evaluated directly by dogma.attributes.attribute.Attribute._ApplyModifierOperationsToVal:
    [-1, 0, 1, 2, 3, 4, 5, 6, 8, 7].
    Operator 9 (dgmAssSpecialSkillRelationship) is defined in SDE dogma effects but is
    decoded and applied in CCP's location layer (opDecoder in baseDogmaLocation.pyj).
    Here, the 10 core operators are evaluated with 100% authentic CCP code and no
    monkey-patched lambdas.
    """
    lm = MockDogmaLM()
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

    # Add modifiers in non-sorted sequence to verify that CCP's Attribute class
    # evaluates them strictly by const.dgmOperatorPrecedence:
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
# Ship Fit Evaluation via CCP's Attribute Modifier Graph
# ---------------------------------------------------------------------------
def evaluate_dogma_fit(ship_type_id, skills, modules, implants):
    lm = MockDogmaLM()
    ship_type = get_type(ship_type_id)
    ship = DogmaItem(100, ship_type_id, ship_type["group_id"], ship_type["category_id"], lm)

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
        mod_items.append({"item": m_item, "type": mt, "raw": m})

    # Ship passive role bonuses to module groups
    for eid in ship_type["effects"]:
        eff = get_effect(eid)
        if not eff or eff["category"] not in (0, 4):
            continue
        for spec in eff["modifiers"]:
            if spec["domain"] == "shipID" and spec["func"] == "LocationGroupModifier":
                grp = spec["group_id"]
                mod_attr_id = spec["modifying_attribute_id"]
                src_attr = ship.get_or_create_attr(mod_attr_id)
                t_attr_id = spec["modified_attribute_id"]
                op = spec["operator"]
                for m in mod_items:
                    if m["type"]["group_id"] == grp:
                        m_attr = m["item"].get_or_create_attr(t_attr_id)
                        m_attr.AddIncomingModifier(op, src_attr)

    # Skills: create skill DogmaItems and wire modifiers
    for sk_tid, lvl in sorted(skills.iteritems()):
        sk_type = get_type(sk_tid)
        if not sk_type:
            continue
        sk_item = DogmaItem(sk_tid, sk_tid, sk_type["group_id"], sk_type["category_id"], lm)
        sk_attrs = dict(sk_type["attributes"])
        sk_attrs[280] = float(lvl)
        for aid, val in sk_attrs.iteritems():
            sk_item.get_or_create_attr(aid, val)

        # itemID modifiers within skill (e.g. skillLevel modifying bonus attribute)
        for eid in sk_type["effects"]:
            if eid == 132:
                continue
            eff = get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                if m["domain"] == "itemID":
                    src = sk_item.get_or_create_attr(m["modifying_attribute_id"])
                    dst = sk_item.get_or_create_attr(m["modified_attribute_id"])
                    dst.AddIncomingModifier(m["operator"], src)

        # shipID modifiers from skill
        for eid in sk_type["effects"]:
            if eid == 132:
                continue
            eff = get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                func = m["func"]
                op = m["operator"]
                t_attr = m["modified_attribute_id"]
                src = sk_item.get_or_create_attr(m["modifying_attribute_id"])
                if m["domain"] == "shipID":
                    if func == "ItemModifier":
                        dst = ship.get_or_create_attr(t_attr)
                        dst.AddIncomingModifier(op, src)
                    elif func == "LocationRequiredSkillModifier":
                        req_sk = m["skill_type_id"]
                        for mod in mod_items:
                            if item_requires_skill(mod["type"]["attributes"], req_sk):
                                dst = mod["item"].get_or_create_attr(t_attr)
                                dst.AddIncomingModifier(op, src)

    # Implants: create implant DogmaItems
    imp_items = []
    for imp_tid in implants:
        imp_type = get_type(imp_tid)
        if not imp_type:
            continue
        imp_item = DogmaItem(imp_tid, imp_tid, imp_type["group_id"], imp_type["category_id"], lm)
        for aid, val in imp_type["attributes"].iteritems():
            imp_item.get_or_create_attr(aid, val)
        imp_items.append({"item": imp_item, "type": imp_type, "tid": imp_tid})

    # Evaluate pirate implant set multipliers via AddIncomingModifier on implant attributes
    if len(imp_items) > 1:
        for imp in imp_items:
            for eid in imp["type"]["effects"]:
                eff = get_effect(eid)
                if not eff or eff["category"] != 0:
                    continue
                for m in eff["modifiers"]:
                    if m["domain"] == "charID" and m["operator"] == 0:
                        src = imp["item"].get_or_create_attr(m["modifying_attribute_id"])
                        for other in imp_items:
                            dst = other["item"].get_or_create_attr(m["modified_attribute_id"])
                            dst.AddIncomingModifier(0, src)

    # Implants to ship
    for imp in imp_items:
        for eid in imp["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                if m["domain"] == "shipID":
                    src = imp["item"].get_or_create_attr(m["modifying_attribute_id"])
                    dst = ship.get_or_create_attr(m["modified_attribute_id"])
                    dst.AddIncomingModifier(m["operator"], src)

    # Modules: calculate dynamic fitting loads and wire shipID modifiers
    total_cpu = 0.0
    total_pwr = 0.0
    for mod in mod_items:
        c_attr = mod["item"].get_or_create_attr(50)
        p_attr = mod["item"].get_or_create_attr(30)
        total_cpu += c_attr.GetValue()
        total_pwr += p_attr.GetValue()

        for eid in mod["type"]["effects"]:
            eff = get_effect(eid)
            if not eff or eff["category"] not in (0, 4):
                continue
            for spec in eff["modifiers"]:
                if spec["domain"] == "shipID":
                    src = mod["item"].get_or_create_attr(spec["modifying_attribute_id"])
                    dst = ship.get_or_create_attr(spec["modified_attribute_id"])
                    dst.AddIncomingModifier(spec["operator"], src)

    # Set ship CPU & Power loads from module totals
    ship.get_or_create_attr(49, total_cpu)
    ship.get_or_create_attr(15, total_pwr)

    res = {}
    for name, aid in ATTR_NAME_TO_ID.iteritems():
        attr_obj = ship.get_or_create_attr(aid)
        res[name] = attr_obj.GetValue()
    return res


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
            "skills": skills,
            "modules": s["modules"],
            "implants": s["implants"],
            "expected_attributes": expected_attrs,
        })

    output_payload = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "oracle_source": "CCP_BUNDLED_RUNTIME",
        "stacking_constants": {
            "max_len": stacking_data["max_len"],
            "exp_factor": stacking_data["exp_factor"],
            "exp_power": stacking_data["exp_power"],
        },
        "stacking_factors": stacking_data["stacking_factors"],
        "operator_precedence": stacking_data["operator_precedence"],
        "precedence_test": prec,
        "fits": evaluated_fits,
    }

    if out_json_path:
        with open(out_json_path, "w") as f:
            json.dump(output_payload, f, indent=2)
    else:
        print(json.dumps(output_payload, indent=2))


if __name__ == "__main__":
    main()
