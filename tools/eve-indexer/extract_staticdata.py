#!/usr/bin/env python3
"""
extract_staticdata.py — Hand-Curated Test-Only Static Data Seed Generator (Pending Phase 1 D-1).

NOTE (S-11): All tables in this script are hand-curated test-only seed literals
(`100` types, `95` attributes, `253` effects, `8` `type_materials` entries,
`12` bloodlines, `36` ancestries, `12` schools, `22` solar systems, `6` jumps,
`2` blueprints, `17` missions) emitted to `docs/index/sde_seed.json` for unit and
integration tests until Phase 1 (`D-1` real SDE importer) replaces it.
"""

import json
import os
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
INDEX_DIR = REPO_ROOT / "docs" / "index"


def build_canonical_sde_seed() -> dict:
    static_catalog_path = INDEX_DIR / "staticdata_catalog.json"
    dogma_catalog_path = INDEX_DIR / "dogma_effects.json"

    static_meta = {}
    if static_catalog_path.exists():
        static_meta = json.loads(static_catalog_path.read_text(encoding="utf-8"))

    dogma_meta = {}
    if dogma_catalog_path.exists():
        dogma_meta = json.loads(dogma_catalog_path.read_text(encoding="utf-8"))

    # 1. Canonical Dogma Attributes (dgmAttributeTypes)
    attributes = [
        {"attribute_id": 4, "name": "mass", "default_value": 1.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 6, "name": "capacitorNeed", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 9, "name": "hp", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 11, "name": "powerOutput", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 12, "name": "lowSlots", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 13, "name": "medSlots", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 14, "name": "hiSlots", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 15, "name": "powerLoad", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 20, "name": "speedFactor", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 30, "name": "power", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 37, "name": "maxVelocity", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 38, "name": "capacity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 48, "name": "cpuOutput", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 49, "name": "cpuLoad", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 50, "name": "cpu", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 51, "name": "speed", "default_value": 1000.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 54, "name": "maxRange", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 55, "name": "rechargeRate", "default_value": 1000.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 64, "name": "damageMultiplier", "default_value": 1.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 68, "name": "shieldBonus", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 70, "name": "agility", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 73, "name": "duration", "default_value": 1000.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 76, "name": "maxTargetRange", "default_value": 25000.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 79, "name": "scanSpeed", "default_value": 1000.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 101, "name": "launcherSlotsLeft", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 102, "name": "turretSlotsLeft", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 103, "name": "warpScrambleRange", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 105, "name": "warpScrambleStrength", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 109, "name": "kineticDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 110, "name": "thermalDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 111, "name": "explosiveDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 113, "name": "emDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 114, "name": "emDamage", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 116, "name": "explosiveDamage", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 117, "name": "kineticDamage", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 118, "name": "thermalDamage", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 158, "name": "falloff", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 160, "name": "trackingSpeed", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 192, "name": "maxLockedTargets", "default_value": 5.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 208, "name": "scanRadarStrength", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 209, "name": "scanLadarStrength", "default_value": 10.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 210, "name": "scanMagnetometricStrength", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 211, "name": "scanGravimetricStrength", "default_value": 0.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 263, "name": "shieldCapacity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 265, "name": "armorHP", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 267, "name": "armorEmDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 268, "name": "armorExplosiveDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 269, "name": "armorKineticDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 270, "name": "armorThermalDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 271, "name": "shieldEmDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 272, "name": "shieldExplosiveDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 273, "name": "shieldKineticDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 274, "name": "shieldThermalDamageResonance", "default_value": 1.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 275, "name": "skillTimeConstant", "default_value": 1.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 283, "name": "droneCapacity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 479, "name": "shieldRechargeRate", "default_value": 1000.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 481, "name": "entityKillBounty", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 482, "name": "capacitorCapacity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 552, "name": "signatureRadius", "default_value": 35.0, "high_is_good": False, "stackable": False},
        {"attribute_id": 564, "name": "scanResolution", "default_value": 400.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 600, "name": "warpSpeedMultiplier", "default_value": 3.0, "high_is_good": True, "stackable": False},
        {"attribute_id": 796, "name": "massAddition", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 1132, "name": "upgradeCapacity", "default_value": 400.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1137, "name": "rigSlots", "default_value": 3.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1153, "name": "upgradeCost", "default_value": 100.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 1179, "name": "heatDissipationRateHi", "default_value": 0.01, "high_is_good": True, "stackable": True},
        {"attribute_id": 1196, "name": "heatDamage", "default_value": 1.4, "high_is_good": False, "stackable": True},
        {"attribute_id": 1211, "name": "heatGenerationMultiplier", "default_value": 1.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 1271, "name": "droneBandwidth", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 128, "name": "chargeSize", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 137, "name": "launcherGroup", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 182, "name": "requiredSkill1", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 183, "name": "requiredSkill2", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 184, "name": "requiredSkill3", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 277, "name": "requiredSkill1Level", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 278, "name": "requiredSkill2Level", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 279, "name": "requiredSkill3Level", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 281, "name": "explosionDelay", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 604, "name": "chargeGroup1", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 653, "name": "aoeVelocity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 654, "name": "aoeCloudSize", "default_value": 0.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 1298, "name": "canFitShipGroup01", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1299, "name": "canFitShipGroup02", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1300, "name": "canFitShipGroup03", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1302, "name": "canFitShipType1", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1303, "name": "canFitShipType2", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 1353, "name": "aoeDamageReductionFactor", "default_value": 1.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 1547, "name": "rigSize", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 866, "name": "jumpDriveConsumptionType", "default_value": 4247.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 867, "name": "jumpDriveRange", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 868, "name": "jumpDriveConsumptionAmount", "default_value": 50.0, "high_is_good": False, "stackable": True},
        {"attribute_id": 2055, "name": "fighterCapacity", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 2215, "name": "fighterSquadronMaxSize", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 2216, "name": "fighterTubes", "default_value": 0.0, "high_is_good": True, "stackable": True},
        {"attribute_id": 790, "name": "reprocessingSkillType", "default_value": 0.0, "high_is_good": True, "stackable": True},
    ]

    # 2. Canonical Dogma Effects (dgmEffects) + 250 indexed effects from dogma_effects.json
    effects = [
        {
            "effect_id": 11,
            "name": "loPower",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 12,
            "name": "hiPower",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 13,
            "name": "medPower",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 16,
            "name": "online",
            "effect_category": 4,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [
                {"domain": "shipID", "func": "ItemModifier", "modified_attribute_id": 49, "modifying_attribute_id": 50, "operator": 2},
                {"domain": "shipID", "func": "ItemModifier", "modified_attribute_id": 15, "modifying_attribute_id": 30, "operator": 2},
            ],
        },
        {
            "effect_id": 34,
            "name": "projectileFired",
            "effect_category": 1,
            "is_offensive": True,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 39,
            "name": "warpScramble",
            "effect_category": 2,
            "is_offensive": True,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 67,
            "name": "moduleBonusAfterburner",
            "effect_category": 1,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [
                {"domain": "shipID", "func": "ItemModifier", "modified_attribute_id": 37, "modifying_attribute_id": 20, "operator": 6},
                {"domain": "shipID", "func": "ItemModifier", "modified_attribute_id": 4, "modifying_attribute_id": 796, "operator": 2},
            ],
        },
        {
            "effect_id": 1851,
            "name": "overdriveSpeedBonus",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [
                {"domain": "shipID", "func": "ItemModifier", "modified_attribute_id": 37, "modifying_attribute_id": 20, "operator": 6},
            ],
        },
        {
            "effect_id": 2663,
            "name": "rigSlot",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 40,
            "name": "launcherFitted",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 42,
            "name": "turretFitted",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
        {
            "effect_id": 3772,
            "name": "subSystem",
            "effect_category": 0,
            "is_offensive": False,
            "is_assistance": False,
            "modifiers": [],
        },
    ]
    existing_effect_ids = {e["effect_id"] for e in effects}
    if isinstance(dogma_meta, dict):
        for se in dogma_meta.get("sample_effects", []):
            eff_id = int(se.get("effectID", 0))
            if 0 < eff_id <= 65535 and eff_id not in existing_effect_ids:
                existing_effect_ids.add(eff_id)
                effects.append({
                    "effect_id": eff_id,
                    "name": str(se.get("effectName", f"effect_{eff_id}")),
                    "effect_category": int(se.get("effectCategory", 0)),
                    "is_offensive": False,
                    "is_assistance": False,
                    "modifiers": [],
                })

    # 3. Canonical Item Types (invTypes + typedogma)
    types = [
        # Ships (Category 6) — Frigates, Corvettes, Industrial, Battleship, Carrier, Capsule
        {
            "type_id": 587, "group_id": 25, "category_id": 6, "name": "Rifter",
            "mass": 1_067_000.0, "volume": 27_289.0, "capacity": 140.0, "radius": 37.0,
            "base_price": 230_000.0, "published": True,
            "default_attributes": {
                "4": 1_067_000.0, "9": 350.0, "11": 41.0, "12": 4.0, "14": 3.0, "13": 3.0,
                "37": 365.0, "38": 140.0, "48": 130.0, "55": 125_000.0, "70": 3.45,
                "76": 27_500.0, "101": 1.0, "102": 3.0, "109": 0.67, "110": 0.67, "111": 0.67, "113": 0.67,
                "192": 4.0, "209": 9.0,
                "263": 450.0, "265": 450.0, "267": 0.4, "268": 0.9, "269": 0.75, "270": 0.65,
                "271": 1.0, "272": 0.5, "273": 0.6, "274": 0.8,
                "479": 625_000.0, "482": 250.0, "552": 35.0, "564": 660.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 588, "group_id": 237, "category_id": 6, "name": "Reaper",
            "mass": 1_128_000.0, "volume": 28_600.0, "capacity": 115.0, "radius": 34.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_128_000.0, "9": 225.0, "11": 30.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 315.0, "38": 115.0, "48": 105.0, "55": 95_000.0, "70": 3.65,
                "101": 2.0, "102": 2.0,
                "263": 250.0, "265": 225.0, "479": 625_000.0, "482": 180.0, "552": 41.0, "564": 550.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 596, "group_id": 237, "category_id": 6, "name": "Impairor",
            "mass": 1_240_000.0, "volume": 27_400.0, "capacity": 120.0, "radius": 35.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_240_000.0, "9": 250.0, "11": 35.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 295.0, "38": 120.0, "48": 95.0, "55": 100_000.0, "70": 3.7,
                "101": 2.0, "102": 2.0,
                "263": 200.0, "265": 275.0, "479": 625_000.0, "482": 210.0, "552": 42.0, "564": 540.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 601, "group_id": 237, "category_id": 6, "name": "Ibis",
            "mass": 1_197_000.0, "volume": 27_600.0, "capacity": 125.0, "radius": 35.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_197_000.0, "9": 220.0, "11": 28.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 280.0, "38": 125.0, "48": 120.0, "55": 95_000.0, "70": 3.8,
                "101": 2.0, "102": 2.0,
                "263": 280.0, "265": 200.0, "479": 625_000.0, "482": 195.0, "552": 44.0, "564": 520.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 606, "group_id": 237, "category_id": 6, "name": "Velator",
            "mass": 1_175_000.0, "volume": 28_100.0, "capacity": 135.0, "radius": 36.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_175_000.0, "9": 265.0, "11": 32.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 300.0, "38": 135.0, "48": 110.0, "55": 95_000.0, "70": 3.7,
                "101": 2.0, "102": 2.0,
                "263": 225.0, "265": 240.0, "283": 10.0, "479": 625_000.0, "482": 190.0, "552": 43.0, "564": 540.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1271": 10.0, "1547": 1.0
            }
        },
        {
            "type_id": 597, "group_id": 25, "category_id": 6, "name": "Punisher",
            "mass": 1_195_000.0, "volume": 24_500.0, "capacity": 135.0, "radius": 35.0,
            "base_price": 250_000.0, "published": True,
            "default_attributes": {
                "4": 1_195_000.0, "9": 400.0, "11": 60.0, "12": 5.0, "13": 2.0, "14": 4.0,
                "37": 325.0, "38": 135.0, "48": 110.0, "55": 140_000.0, "70": 3.6,
                "101": 2.0, "102": 4.0,
                "263": 350.0, "265": 650.0, "479": 625_000.0, "482": 345.0, "552": 36.0, "564": 620.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 603, "group_id": 25, "category_id": 6, "name": "Merlin",
            "mass": 1_140_000.0, "volume": 26_000.0, "capacity": 145.0, "radius": 36.0,
            "base_price": 240_000.0, "published": True,
            "default_attributes": {
                "4": 1_140_000.0, "9": 375.0, "11": 43.0, "12": 3.0, "13": 4.0, "14": 3.0,
                "37": 310.0, "38": 145.0, "48": 155.0, "55": 135_000.0, "70": 3.7,
                "101": 3.0, "102": 3.0,
                "263": 650.0, "265": 350.0, "479": 625_000.0, "482": 290.0, "552": 39.0, "564": 600.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1547": 1.0
            }
        },
        {
            "type_id": 594, "group_id": 25, "category_id": 6, "name": "Incursus",
            "mass": 1_120_000.0, "volume": 25_500.0, "capacity": 140.0, "radius": 38.0,
            "base_price": 245_000.0, "published": True,
            "default_attributes": {
                "4": 1_120_000.0, "9": 450.0, "11": 48.0, "12": 4.0, "13": 3.0, "14": 3.0,
                "37": 340.0, "38": 140.0, "48": 135.0, "55": 130_000.0, "70": 3.55,
                "101": 1.0, "102": 3.0,
                "263": 400.0, "265": 500.0, "283": 5.0, "479": 625_000.0, "482": 300.0, "552": 37.0, "564": 640.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1271": 5.0, "1547": 1.0
            }
        },
        {
            "type_id": 32880, "group_id": 1283, "category_id": 6, "name": "Venture",
            "mass": 1_200_000.0, "volume": 29_200.0, "capacity": 5000.0, "radius": 40.0,
            "base_price": 300_000.0, "published": True,
            "default_attributes": {
                "4": 1_200_000.0, "9": 400.0, "11": 35.0, "12": 1.0, "13": 3.0, "14": 3.0,
                "37": 335.0, "38": 5000.0, "48": 240.0, "55": 125_000.0, "70": 3.6,
                "101": 2.0, "102": 2.0, "105": -2.0, "263": 550.0, "265": 350.0, "283": 10.0,
                "479": 625_000.0, "482": 250.0, "552": 40.0, "564": 700.0, "600": 5.0,
                "1132": 400.0, "1137": 3.0, "1271": 10.0, "1547": 1.0
            }
        },
        {
            "type_id": 638, "group_id": 27, "category_id": 6, "name": "Raven",
            "mass": 99_300_000.0, "volume": 470_000.0, "capacity": 830.0, "radius": 320.0,
            "base_price": 210_000_000.0, "published": True,
            "default_attributes": {
                "4": 99_300_000.0, "9": 7040.0, "11": 11_000.0, "12": 5.0, "13": 7.0, "14": 7.0,
                "37": 115.0, "38": 830.0, "48": 750.0, "55": 1_000_000.0, "70": 0.125,
                "101": 6.0, "102": 4.0, "263": 7700.0, "265": 6380.0,
                "267": 0.5, "268": 0.9, "269": 0.75, "270": 0.55,
                "271": 1.0, "272": 0.5, "273": 0.6, "274": 0.8,
                "283": 75.0,
                "479": 2_500_000.0, "482": 5500.0, "552": 415.0, "564": 115.0, "600": 3.0,
                "1132": 400.0, "1137": 3.0, "1271": 50.0, "1547": 3.0
            }
        },
        {
            "type_id": 24483, "group_id": 547, "category_id": 6, "name": "Archon",
            "mass": 1_025_000_000.0, "volume": 16_250_000.0, "capacity": 2500.0, "radius": 1500.0,
            "base_price": 3_500_000_000.0, "published": True,
            "default_attributes": {
                "4": 1_025_000_000.0, "9": 95_000.0, "11": 650_000.0, "12": 7.0, "13": 4.0, "14": 5.0,
                "37": 85.0, "38": 2500.0, "48": 850.0, "55": 2_500_000.0, "70": 0.04,
                "101": 0.0, "102": 0.0,
                "263": 80_000.0, "265": 140_000.0, "479": 5_000_000.0, "482": 65_000.0,
                "552": 2950.0, "564": 75.0, "600": 1.5,
                "866": 4247.0, "867": 7.0, "868": 50.0,
                "1132": 400.0, "1137": 3.0, "1547": 4.0, "2055": 15_000.0, "2216": 3.0
            }
        },
        {
            "type_id": 670, "group_id": 29, "category_id": 6, "name": "Capsule",
            "mass": 16_000.0, "volume": 1000.0, "capacity": 0.0, "radius": 18.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 16_000.0, "9": 350.0, "12": 0.0, "13": 0.0, "14": 0.0,
                "37": 210.0, "70": 0.06, "101": 0.0, "102": 0.0,
                "263": 250.0, "265": 250.0,
                "482": 100.0, "552": 25.0, "600": 3.0,
                "1132": 0.0, "1137": 0.0, "1547": 0.0
            }
        },
        # Modules (Category 7)
        {
            "type_id": 438, "group_id": 46, "category_id": 7, "name": "1MN Afterburner II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 500_000.0, "published": True,
            "effect_ids": [13, 16, 67],
            "default_attributes": {
                "4": 1.0, "6": 20.0, "20": 135.0, "30": 15.0, "50": 25.0, "73": 7500.0,
                "182": 3449.0, "277": 3.0, "183": 3452.0, "278": 1.0,
                "796": 500_000.0, "1196": 1.4
            }
        },
        {
            "type_id": 1248, "group_id": 762, "category_id": 7, "name": "Overdrive Injector System II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 350_000.0, "published": True,
            "effect_ids": [11, 16, 1851],
            "default_attributes": {
                "20": 12.5, "30": 1.0, "50": 15.0,
                "182": 3449.0, "277": 3.0
            }
        },
        {
            "type_id": 2889, "group_id": 55, "category_id": 7, "name": "200mm AutoCannon II",
            "mass": 15.0, "volume": 5.0, "capacity": 5.0, "radius": 20.0,
            "base_price": 600_000.0, "published": True,
            "effect_ids": [12, 16, 34, 42],
            "default_attributes": {
                "30": 4.0, "50": 9.0, "51": 3125.0, "54": 1200.0, "64": 3.465,
                "128": 1.0, "158": 4800.0, "160": 0.315,
                "182": 3302.0, "277": 4.0, "183": 3300.0, "278": 2.0,
                "604": 83.0, "1196": 0.02
            }
        },
        {
            "type_id": 2410, "group_id": 509, "category_id": 7, "name": "Light Missile Launcher II",
            "mass": 1.0, "volume": 5.0, "capacity": 1.29, "radius": 1.0,
            "base_price": 750_000.0, "published": True,
            "effect_ids": [12, 16, 40],
            "default_attributes": {
                "30": 9.0, "50": 28.0, "51": 12000.0, "604": 384.0,
                "182": 3319.0, "277": 3.0, "183": 3321.0, "278": 4.0
            }
        },
        {
            "type_id": 2420, "group_id": 508, "category_id": 7, "name": "Torpedo Launcher II",
            "mass": 1.0, "volume": 20.0, "capacity": 15.0, "radius": 1.0,
            "base_price": 2_200_000.0, "published": True,
            "effect_ids": [12, 16, 40],
            "default_attributes": {
                "30": 1375.0, "50": 66.0, "51": 10500.0, "604": 89.0,
                "182": 3319.0, "277": 5.0, "183": 3325.0, "278": 5.0, "184": 20212.0, "279": 1.0
            }
        },
        {
            "type_id": 448, "group_id": 52, "category_id": 7, "name": "Warp Scrambler II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 800_000.0, "published": True,
            "effect_ids": [13, 16, 39],
            "default_attributes": {
                "6": 6.0, "30": 1.0, "50": 30.0, "54": 9000.0, "73": 5000.0, "105": 2.0,
                "182": 3449.0, "277": 2.0
            }
        },
        {
            "type_id": 2048, "group_id": 60, "category_id": 7, "name": "Damage Control II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 550_000.0, "published": True,
            "effect_ids": [11, 16],
            "default_attributes": {"30": 1.0, "50": 30.0, "182": 3327.0, "277": 1.0}
        },
        {
            "type_id": 31788, "group_id": 774, "category_id": 7, "name": "Small Core Defense Field Extender I",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 900_000.0, "published": True,
            "effect_ids": [2663],
            "default_attributes": {"68": 15.0, "1153": 50.0, "1547": 1.0}
        },
        {
            "type_id": 483, "group_id": 54, "category_id": 7, "name": "Miner I",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 50_000.0, "published": True,
            "effect_ids": [12, 16, 42],
            "default_attributes": {
                "6": 10.0, "30": 2.0, "50": 30.0, "54": 10_000.0, "73": 60_000.0,
                "182": 3380.0, "277": 1.0
            }
        },
        {
            "type_id": 54800, "group_id": 4060, "category_id": 7, "name": "Small Vorton Projector I",
            "mass": 1.0, "volume": 5.0, "capacity": 5.0, "radius": 1.0,
            "base_price": 2_500_000.0, "published": True,
            "effect_ids": [12, 16, 42],
            # eve-wiki/articles/Missile_mechanics.md:209 specifies Small Vorton Projector DRF (1353) = 0.40 (0.50 Medium, 0.70 Large)
            # eve-wiki/articles/Vorton_Projectors.md:42 specifies up to 9 additional arc targets (2104 = 9.0) within 10 km of primary
            "default_attributes": {
                "30": 8.0, "50": 20.0, "51": 6000.0, "54": 22_460.0, "64": 1.0,
                "128": 1.0, "604": 4061.0, "653": 190.0, "654": 43.0, "1353": 0.40, "2104": 9.0
            }
        },
        {
            "type_id": 47936, "group_id": 1986, "category_id": 7, "name": "Heavy Entropic Disintegrator II",
            "mass": 1.0, "volume": 20.0, "capacity": 10.0, "radius": 1.0,
            "base_price": 12_000_000.0, "published": True,
            "effect_ids": [12, 16, 42],
            "default_attributes": {
                "30": 900.0, "50": 55.0, "51": 3500.0, "54": 48_000.0, "64": 2.8,
                "128": 2.0, "160": 180.0, "604": 1987.0,
                "182": 3300.0, "277": 4.0, "183": 33094.0, "278": 5.0,
                "1302": 47270.0, "1303": 49711.0
            }
        },
        {
            "type_id": 84010, "group_id": 4500, "category_id": 7, "name": "Small Breacher Pod Launcher I",
            "mass": 1.0, "volume": 5.0, "capacity": 10.0, "radius": 1.0,
            "base_price": 4_000_000.0, "published": True,
            "effect_ids": [12, 16, 40],
            "default_attributes": {
                "30": 6.0, "50": 18.0, "51": 24000.0, "54": 4000.0,
                "604": 4501.0
            }
        },
        # Minerals, Ores, Charges, Fuel Blocks, Injectors, Quantum Cores & Clone Soldier Tags
        {"type_id": 34, "group_id": 18, "category_id": 4, "name": "Tritanium", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5.0, "published": True, "default_attributes": {}},
        {"type_id": 35, "group_id": 18, "category_id": 4, "name": "Pyerite", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 12.0, "published": True, "default_attributes": {}},
        {"type_id": 36, "group_id": 18, "category_id": 4, "name": "Mexallon", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 75.0, "published": True, "default_attributes": {}},
        {"type_id": 37, "group_id": 18, "category_id": 4, "name": "Isogen", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 150.0, "published": True, "default_attributes": {}},
        {"type_id": 38, "group_id": 18, "category_id": 4, "name": "Nocxium", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 600.0, "published": True, "default_attributes": {}},
        {"type_id": 39, "group_id": 18, "category_id": 4, "name": "Zydrine", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 1200.0, "published": True, "default_attributes": {}},
        {"type_id": 40, "group_id": 18, "category_id": 4, "name": "Megacyte", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 2400.0, "published": True, "default_attributes": {}},
        {"type_id": 4247, "group_id": 1136, "category_id": 4, "name": "Helium Fuel Block", "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {}},
        {"type_id": 1230, "group_id": 462, "category_id": 25, "name": "Veldspar", "portion_size": 100, "mass": 1.0, "volume": 0.1, "capacity": 0.0, "radius": 1.0, "base_price": 15.0, "published": True, "default_attributes": {"790": 12195.0}},
        {"type_id": 28432, "group_id": 462, "category_id": 25, "name": "Compressed Veldspar", "portion_size": 1, "mass": 1.0, "volume": 0.001, "capacity": 0.0, "radius": 1.0, "base_price": 1500.0, "published": True, "default_attributes": {"790": 12195.0}},
        {"type_id": 1228, "group_id": 460, "category_id": 25, "name": "Scordite", "portion_size": 100, "mass": 1.0, "volume": 0.15, "capacity": 0.0, "radius": 1.0, "base_price": 22.0, "published": True, "default_attributes": {"790": 12193.0}},
        {"type_id": 28429, "group_id": 460, "category_id": 25, "name": "Compressed Scordite", "portion_size": 1, "mass": 1.0, "volume": 0.0015, "capacity": 0.0, "radius": 1.0, "base_price": 2200.0, "published": True, "default_attributes": {"790": 12193.0}},
        {"type_id": 18, "group_id": 458, "category_id": 25, "name": "Plagioclase", "portion_size": 100, "mass": 1.0, "volume": 0.35, "capacity": 0.0, "radius": 1.0, "base_price": 30.0, "published": True, "default_attributes": {"790": 12191.0}},
        {
            "type_id": 22989, "group_id": 83, "category_id": 8, "name": "Republic Fleet Phased Plasma S",
            "mass": 0.01, "volume": 0.0025, "capacity": 0.0, "radius": 1.0, "base_price": 800.0, "published": True,
            "default_attributes": {"117": 2.3, "118": 11.5, "128": 1.0, "137": 55.0, "182": 3302.0, "277": 1.0}
        },
        {
            "type_id": 211, "group_id": 384, "category_id": 8, "name": "Inferno Light Missile",
            "mass": 70.0, "volume": 0.015, "capacity": 0.0, "radius": 300.0, "base_price": 50.0, "published": True,
            "default_attributes": {
                "37": 3750.0, "118": 83.0, "137": 509.0,
                "182": 3321.0, "277": 1.0, "183": 3319.0, "278": 1.0,
                "281": 5000.0, "653": 170.0, "654": 40.0, "1353": 0.604
            }
        },
        {
            "type_id": 2510, "group_id": 89, "category_id": 8, "name": "Inferno Torpedo",
            "mass": 1500.0, "volume": 0.05, "capacity": 0.0, "radius": 300.0, "base_price": 500.0, "published": True,
            "default_attributes": {
                "37": 1500.0, "118": 450.0, "137": 508.0,
                "182": 3325.0, "277": 1.0, "183": 3319.0, "278": 1.0,
                "281": 6000.0, "653": 78.0, "654": 405.0, "1353": 0.944
            }
        },
        {
            "type_id": 54807, "group_id": 4061, "category_id": 8, "name": "GalvaSurge Condenser Pack S",
            "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 500.0, "published": True,
            "default_attributes": {"114": 116.0, "117": 17.0, "128": 1.0, "137": 4060.0}
        },
        {
            "type_id": 47910, "group_id": 1987, "category_id": 8, "name": "Baryon Exotic Plasma M",
            "mass": 1.0, "volume": 0.02, "capacity": 0.0, "radius": 1.0, "base_price": 600.0, "published": True,
            "default_attributes": {"116": 129.0, "118": 180.0, "128": 2.0, "137": 1986.0, "182": 33094.0, "277": 1.0}
        },
        {
            "type_id": 84015, "group_id": 4501, "category_id": 8, "name": "SCARAB Breacher Pod S",
            "mass": 100.0, "volume": 0.25, "capacity": 0.0, "radius": 50.0, "base_price": 1500.0, "published": True,
            "default_attributes": {"37": 2000.0, "116": 200.0, "137": 4500.0, "281": 2000.0}
        },
        {"type_id": 40520, "group_id": 1739, "category_id": 5, "name": "Large Skill Injector", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 850_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 55001, "group_id": 1950, "category_id": 91, "name": "Rifter Crimson Valkyrie SKIN (Permanent)", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 10_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 56201, "group_id": 4088, "category_id": 17, "name": "Astrahus Upwell Quantum Core", "mass": 5000.0, "volume": 100.0, "capacity": 0.0, "radius": 1.0, "base_price": 600_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35661, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Trainer Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35662, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Recruiter Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 15_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35663, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Transporter Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 25_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35664, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Negotiator Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 45_000_000.0, "published": True, "default_attributes": {}},
        # Skills (Category 16)
        {"type_id": 3327, "group_id": 257, "category_id": 16, "name": "Spaceship Command", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3329, "group_id": 257, "category_id": 16, "name": "Minmatar Frigate", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 40_000.0, "published": True, "default_attributes": {"275": 2.0, "182": 3327.0, "277": 1.0}},
        {"type_id": 3449, "group_id": 275, "category_id": 16, "name": "Navigation", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3452, "group_id": 275, "category_id": 16, "name": "Acceleration Control", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 120_000.0, "published": True, "default_attributes": {"275": 4.0, "182": 3449.0, "277": 3.0}},
        {"type_id": 3300, "group_id": 255, "category_id": 16, "name": "Gunnery", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3302, "group_id": 255, "category_id": 16, "name": "Small Projectile Turret", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0, "182": 3300.0, "277": 1.0}},
        {"type_id": 3319, "group_id": 256, "category_id": 16, "name": "Missile Launcher Operation", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3321, "group_id": 256, "category_id": 16, "name": "Light Missiles", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 40_000.0, "published": True, "default_attributes": {"275": 2.0, "182": 3319.0, "277": 1.0}},
        {"type_id": 3325, "group_id": 256, "category_id": 16, "name": "Torpedoes", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 150_000.0, "published": True, "default_attributes": {"275": 4.0, "182": 3319.0, "277": 4.0}},
        {"type_id": 20212, "group_id": 256, "category_id": 16, "name": "Torpedo Specialization", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 600_000.0, "published": True, "default_attributes": {"275": 8.0, "182": 3325.0, "277": 5.0}},
        {"type_id": 33094, "group_id": 255, "category_id": 16, "name": "Medium Precursor Weapon", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 2_000_000.0, "published": True, "default_attributes": {"275": 5.0, "182": 3300.0, "277": 2.0}},
        {"type_id": 2505, "group_id": 268, "category_id": 16, "name": "Command Center Upgrades", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 250_000.0, "published": True, "default_attributes": {"275": 4.0}},
        {"type_id": 3380, "group_id": 268, "category_id": 16, "name": "Industry", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3385, "group_id": 268, "category_id": 16, "name": "Reprocessing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 30_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3356, "group_id": 278, "category_id": 16, "name": "Negotiation", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 120_000.0, "published": True, "default_attributes": {"275": 2.0}},
        {"type_id": 3388, "group_id": 268, "category_id": 16, "name": "Advanced Industry", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 200_000.0, "published": True, "default_attributes": {"275": 3.0, "182": 3380.0, "277": 3.0}},
        {"type_id": 3389, "group_id": 268, "category_id": 16, "name": "Reprocessing Efficiency", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 150_000.0, "published": True, "default_attributes": {"275": 3.0, "182": 3385.0, "277": 5.0}},
        {"type_id": 12191, "group_id": 1218, "category_id": 16, "name": "Plagioclase Processing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 50_000.0, "published": True, "default_attributes": {"275": 1.0, "182": 3385.0, "277": 4.0}},
        {"type_id": 12193, "group_id": 1218, "category_id": 16, "name": "Scordite Processing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 50_000.0, "published": True, "default_attributes": {"275": 1.0, "182": 3385.0, "277": 4.0}},
        {"type_id": 12195, "group_id": 1218, "category_id": 16, "name": "Veldspar Processing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 50_000.0, "published": True, "default_attributes": {"275": 1.0, "182": 3385.0, "277": 4.0}},
        {"type_id": 12196, "group_id": 1218, "category_id": 16, "name": "Scrapmetal Processing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 150_000.0, "published": True, "default_attributes": {"275": 5.0, "182": 3389.0, "277": 5.0}},
        {"type_id": 3443, "group_id": 274, "category_id": 16, "name": "Trade", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3446, "group_id": 274, "category_id": 16, "name": "Broker Relations", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {"275": 2.0, "182": 3443.0, "277": 2.0}},
        {"type_id": 3893, "group_id": 273, "category_id": 16, "name": "Security Connections", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 160_000.0, "published": True, "default_attributes": {"275": 2.0}},
        {"type_id": 11448, "group_id": 270, "category_id": 16, "name": "Minmatar Starship Engineering", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 500_000.0, "published": True, "default_attributes": {"275": 5.0}},
        {"type_id": 11452, "group_id": 270, "category_id": 16, "name": "Mechanical Engineering", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 500_000.0, "published": True, "default_attributes": {"275": 5.0}},
        {"type_id": 16622, "group_id": 274, "category_id": 16, "name": "Accounting", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5_000_000.0, "published": True, "default_attributes": {"275": 3.0, "182": 3443.0, "277": 4.0}},
        {"type_id": 21791, "group_id": 270, "category_id": 16, "name": "Minmatar Encryption Methods", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 1_500_000.0, "published": True, "default_attributes": {"275": 4.0}},
        {"type_id": 24242, "group_id": 272, "category_id": 16, "name": "Infomorph Psychology", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 50_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 28164, "group_id": 269, "category_id": 16, "name": "Thermodynamics", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 3_500_000.0, "published": True, "default_attributes": {"275": 3.0}},
        {"type_id": 33407, "group_id": 272, "category_id": 16, "name": "Infomorph Synchronizing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5_000_000.0, "published": True, "default_attributes": {"275": 2.0, "182": 24242.0, "277": 1.0}},
        {"type_id": 20172, "group_id": 333, "category_id": 17, "name": "Datacore - Minmatar Starship Engineering", "mass": 1.0, "volume": 0.1, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {}},
        {"type_id": 20424, "group_id": 333, "category_id": 17, "name": "Datacore - Mechanical Engineering", "mass": 1.0, "volume": 0.1, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {}},
        {"type_id": 3645, "group_id": 1042, "category_id": 43, "name": "Water", "mass": 1.0, "volume": 0.38, "capacity": 0.0, "radius": 1.0, "base_price": 400.0, "published": True, "default_attributes": {}},
        {"type_id": 23055, "group_id": 1652, "category_id": 87, "name": "Templar I", "mass": 20_000.0, "volume": 1000.0, "capacity": 0.0, "radius": 18.0, "base_price": 1_200_000.0, "published": True, "default_attributes": {"9": 450.0, "37": 1250.0, "263": 600.0, "2215": 9.0}},
        {"type_id": 11371, "group_id": 324, "category_id": 6, "name": "Wolf", "mass": 1_250_000.0, "volume": 27_200.0, "capacity": 140.0, "radius": 35.0, "base_price": 24_000_000.0, "published": True, "default_attributes": {"4": 1_250_000.0, "9": 650.0, "11": 52.0, "12": 5.0, "13": 2.0, "14": 4.0, "37": 370.0, "38": 140.0, "48": 160.0, "70": 3.2, "101": 1.0, "102": 4.0, "263": 500.0, "265": 800.0, "482": 350.0, "552": 33.0, "600": 5.0, "1132": 400.0, "1137": 2.0, "1547": 1.0}},
        # Blueprints (Category 9)
        {"type_id": 687, "group_id": 105, "category_id": 9, "name": "Rifter Blueprint", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 2_500_000.0, "published": True, "default_attributes": {}},
        {"type_id": 11372, "group_id": 105, "category_id": 9, "name": "Wolf Blueprint", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 25_000_000.0, "published": False, "default_attributes": {}},
        # Mutaplasmids & Abyssal Dynamic Items
        {"type_id": 47702, "group_id": 1979, "category_id": 5, "name": "Decayed 1MN Afterburner Mutaplasmid", "mass": 0.1, "volume": 1.0, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {}},
        {"type_id": 47740, "group_id": 46, "category_id": 7, "name": "Abyssal 1MN Afterburner", "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0, "base_price": 1_500_000.0, "published": False, "effect_ids": [13, 16, 67], "default_attributes": {"4": 1.0, "6": 20.0, "20": 135.0, "30": 15.0, "50": 25.0, "73": 7500.0, "182": 3449.0, "277": 3.0, "183": 3452.0, "278": 1.0, "796": 500_000.0}},
        # Structures & Deployables (Category 65 & 22)
        {"type_id": 35832, "group_id": 1657, "category_id": 65, "name": "Astrahus", "mass": 500_000_000.0, "volume": 8000.0, "capacity": 100_000.0, "radius": 10_000.0, "base_price": 800_000_000.0, "published": True, "default_attributes": {"9": 1_800_000.0, "263": 1_800_000.0, "265": 1_800_000.0}},
        {"type_id": 35840, "group_id": 2016, "category_id": 65, "name": "Pharolux Cyno Beacon", "mass": 15_000_000.0, "volume": 4000.0, "capacity": 25_000.0, "radius": 1500.0, "base_price": 500_000_000.0, "published": True, "default_attributes": {"9": 500_000.0, "263": 500_000.0, "265": 500_000.0}},
        {"type_id": 33474, "group_id": 1250, "category_id": 22, "name": "Mobile Tractor Unit", "mass": 5000.0, "volume": 100.0, "capacity": 27_000.0, "radius": 25.0, "base_price": 8_000_000.0, "published": True, "default_attributes": {"9": 25_000.0, "263": 25_000.0}},
        {"type_id": 32882, "group_id": 1246, "category_id": 22, "name": "Encounter Surveillance System", "mass": 100_000.0, "volume": 500.0, "capacity": 0.0, "radius": 120.0, "base_price": 50_000_000.0, "published": True, "default_attributes": {"9": 500_000.0}},
        {"type_id": 81140, "group_id": 4700, "category_id": 65, "name": "Orbital Skyhook", "mass": 200_000_000.0, "volume": 10_000.0, "capacity": 500_000.0, "radius": 3500.0, "base_price": 1_200_000_000.0, "published": True, "default_attributes": {"9": 2_500_000.0}},
        {"type_id": 85230, "group_id": 4800, "category_id": 22, "name": "Mercenary Den", "mass": 50_000.0, "volume": 100.0, "capacity": 10_000.0, "radius": 150.0, "base_price": 75_000_000.0, "published": True, "default_attributes": {"9": 150_000.0}},
        # NPC Entities & Wrecks
        {"type_id": 11001, "group_id": 555, "category_id": 11, "name": "Guristas Pithi Invader", "mass": 1_000_000.0, "volume": 25_000.0, "capacity": 100.0, "radius": 35.0, "base_price": 15_000.0, "published": False, "default_attributes": {"9": 50.0, "37": 310.0, "263": 50.0, "265": 50.0, "481": 100_000.0, "552": 38.0}},
        {"type_id": 11003, "group_id": 301, "category_id": 11, "name": "CONCORD Police Commander", "mass": 50_000_000.0, "volume": 150_000.0, "capacity": 500.0, "radius": 150.0, "base_price": 0.0, "published": False, "default_attributes": {"9": 100_000.0, "37": 1500.0, "263": 100_000.0, "265": 100_000.0, "552": 100.0}},
        {"type_id": 24445, "group_id": 649, "category_id": 2, "name": "Courier Plastic Wrap", "mass": 100.0, "volume": 10.0, "capacity": 1000.0, "radius": 5.0, "base_price": 0.0, "published": False, "default_attributes": {"9": 100.0}},
        {"type_id": 26557, "group_id": 186, "category_id": 2, "name": "Small Minmatar Frigate Wreck", "mass": 500_000.0, "volume": 5000.0, "capacity": 1000.0, "radius": 30.0, "base_price": 0.0, "published": False, "default_attributes": {"9": 500.0}},
    ]

    # 4. Races, Bloodlines, Ancestries (all 36 playable from chrAncestries.sql.gz), Schools (chrSchools.sql.gz)
    races = [
        {"race_id": 1, "name": "Caldari", "corvette_type_id": 601},
        {"race_id": 2, "name": "Minmatar", "corvette_type_id": 588},
        {"race_id": 4, "name": "Amarr", "corvette_type_id": 596},
        {"race_id": 8, "name": "Gallente", "corvette_type_id": 606},
    ]
    bloodlines = [
        {"bloodline_id": 1, "race_id": 1, "name": "Deteis", "corporation_id": 1000006},
        {"bloodline_id": 2, "race_id": 1, "name": "Civire", "corporation_id": 1000006},
        {"bloodline_id": 11, "race_id": 1, "name": "Achura", "corporation_id": 1000006},
        {"bloodline_id": 3, "race_id": 2, "name": "Sebiestor", "corporation_id": 1000125},
        {"bloodline_id": 4, "race_id": 2, "name": "Brutor", "corporation_id": 1000125},
        {"bloodline_id": 14, "race_id": 2, "name": "Vherokior", "corporation_id": 1000125},
        {"bloodline_id": 5, "race_id": 4, "name": "Amarr", "corporation_id": 1000077},
        {"bloodline_id": 6, "race_id": 4, "name": "Ni-Kunni", "corporation_id": 1000077},
        {"bloodline_id": 13, "race_id": 4, "name": "Khanid", "corporation_id": 1000077},
        {"bloodline_id": 7, "race_id": 8, "name": "Gallente", "corporation_id": 1000115},
        {"bloodline_id": 8, "race_id": 8, "name": "Intaki", "corporation_id": 1000115},
        {"bloodline_id": 12, "race_id": 8, "name": "Jin-Mei", "corporation_id": 1000115},
    ]
    ancestries = [
        {"ancestry_id": 1, "bloodline_id": 5, "name": "Liberal Holders"},
        {"ancestry_id": 2, "bloodline_id": 5, "name": "Wealthy Commoners"},
        {"ancestry_id": 3, "bloodline_id": 5, "name": "Religious Reclaimers"},
        {"ancestry_id": 4, "bloodline_id": 6, "name": "Free Merchants"},
        {"ancestry_id": 5, "bloodline_id": 6, "name": "Border Runners"},
        {"ancestry_id": 6, "bloodline_id": 6, "name": "Navy Veterans"},
        {"ancestry_id": 7, "bloodline_id": 2, "name": "Entrepreneurs"},
        {"ancestry_id": 8, "bloodline_id": 2, "name": "Mercs"},
        {"ancestry_id": 9, "bloodline_id": 2, "name": "Dissenters"},
        {"ancestry_id": 10, "bloodline_id": 1, "name": "Merchandisers"},
        {"ancestry_id": 11, "bloodline_id": 1, "name": "Scientists"},
        {"ancestry_id": 12, "bloodline_id": 1, "name": "Tube Child"},
        {"ancestry_id": 13, "bloodline_id": 7, "name": "Activists"},
        {"ancestry_id": 14, "bloodline_id": 7, "name": "Miners"},
        {"ancestry_id": 15, "bloodline_id": 7, "name": "Immigrants"},
        {"ancestry_id": 16, "bloodline_id": 8, "name": "Artists"},
        {"ancestry_id": 17, "bloodline_id": 8, "name": "Diplomats"},
        {"ancestry_id": 18, "bloodline_id": 8, "name": "Reborn"},
        {"ancestry_id": 19, "bloodline_id": 3, "name": "Tinkerers"},
        {"ancestry_id": 20, "bloodline_id": 3, "name": "Traders"},
        {"ancestry_id": 21, "bloodline_id": 3, "name": "Rebels"},
        {"ancestry_id": 22, "bloodline_id": 4, "name": "Workers"},
        {"ancestry_id": 23, "bloodline_id": 4, "name": "Tribal Traditionalists"},
        {"ancestry_id": 24, "bloodline_id": 4, "name": "Slave Child"},
        {"ancestry_id": 31, "bloodline_id": 11, "name": "Inventors"},
        {"ancestry_id": 32, "bloodline_id": 11, "name": "Monks"},
        {"ancestry_id": 33, "bloodline_id": 11, "name": "Stargazers"},
        {"ancestry_id": 34, "bloodline_id": 12, "name": "Sang Do Caste"},
        {"ancestry_id": 35, "bloodline_id": 12, "name": "Saan Go Caste"},
        {"ancestry_id": 36, "bloodline_id": 12, "name": "Jing Ko Caste"},
        {"ancestry_id": 37, "bloodline_id": 13, "name": "Cyber Knights"},
        {"ancestry_id": 38, "bloodline_id": 13, "name": "Unionists"},
        {"ancestry_id": 39, "bloodline_id": 13, "name": "Zealots"},
        {"ancestry_id": 40, "bloodline_id": 14, "name": "Drifters"},
        {"ancestry_id": 41, "bloodline_id": 14, "name": "Mystics"},
        {"ancestry_id": 42, "bloodline_id": 14, "name": "Retailers"},
    ]
    schools = [
        {"school_id": 11, "race_id": 4, "corporation_id": 1000166, "station_id": 60014629, "solar_system_id": 30003489, "name": "Imperial Academy"},
        {"school_id": 12, "race_id": 4, "corporation_id": 1000165, "station_id": 60014599, "solar_system_id": 30005042, "name": "Hedion University"},
        {"school_id": 13, "race_id": 4, "corporation_id": 1000077, "station_id": 60007702, "solar_system_id": 30001672, "name": "Royal Amarr Institute"},
        {"school_id": 14, "race_id": 2, "corporation_id": 1000170, "station_id": 60014749, "solar_system_id": 30002547, "name": "Republic Military School"},
        {"school_id": 15, "race_id": 2, "corporation_id": 1000171, "station_id": 60014779, "solar_system_id": 30002505, "name": "Republic University"},
        {"school_id": 16, "race_id": 2, "corporation_id": 1000172, "station_id": 60014809, "solar_system_id": 30003410, "name": "Pator Tech School"},
        {"school_id": 17, "race_id": 1, "corporation_id": 1000167, "station_id": 60014659, "solar_system_id": 30000141, "name": "State War Academy"},
        {"school_id": 18, "race_id": 1, "corporation_id": 1000045, "station_id": 60004450, "solar_system_id": 30001392, "name": "Science and Trade Institute"},
        {"school_id": 19, "race_id": 1, "corporation_id": 1000044, "station_id": 60004420, "solar_system_id": 30001407, "name": "School of Applied Knowledge"},
        {"school_id": 20, "race_id": 8, "corporation_id": 1000168, "station_id": 60014689, "solar_system_id": 30004971, "name": "Federal Navy Academy"},
        {"school_id": 21, "race_id": 8, "corporation_id": 1000115, "station_id": 60011566, "solar_system_id": 30002715, "name": "University of Caille"},
        {"school_id": 22, "race_id": 8, "corporation_id": 1000169, "station_id": 60014719, "solar_system_id": 30005305, "name": "Center for Advanced Studies"},
    ]

    # 5. Solar Systems, Jumps & Content
    solar_systems = [
        {
            "solar_system_id": 30000142, "constellation_id": 20000020, "region_id": 10000002,
            "name": "Jita", "security_status": 0.9459, "radius": 3.592e12,
            "galactic_pos_ly": [-13.642, 6.422, 12.417],
            "planet_ids": [40009076, 40009077, 40009079, 40009082, 40009114, 40009131, 40009142, 40009151],
            "stations": [
                {
                    "station_id": 60003760, "station_type_id": 1529,
                    "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
                    "position": [-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0],
                    "owner_corporation_id": 1000035, "owner_faction_id": 500001
                }
            ]
        },
        {
            "solar_system_id": 30000141, "constellation_id": 20000020, "region_id": 10000002,
            "name": "Kisogo", "security_status": 1.0, "radius": 1.389e12,
            "galactic_pos_ly": [-14.384, 7.130, 9.427],
            "planet_ids": [40009050, 40009055, 40009060],
            "stations": [
                {
                    "station_id": 60014659, "station_type_id": 1529,
                    "name": "Kisogo VII - State War Academy",
                    "position": [983_458_160_640.0, -119_222_476_800.0, -1_157_778_677_760.0],
                    "owner_corporation_id": 1000167, "owner_faction_id": 500001
                }
            ]
        },
        {
            "solar_system_id": 30001392, "constellation_id": 20000204, "region_id": 10000016,
            "name": "Amsen", "security_status": 1.0, "radius": 7.624e11,
            "galactic_pos_ly": [-15.379, 10.739, 14.548],
            "planet_ids": [40088101, 40088105],
            "stations": [
                {
                    "station_id": 60004450, "station_type_id": 1529,
                    "name": "Amsen VI - Moon 1 - Science and Trade Institute School",
                    "position": [273_625_620_480.0, -12_937_666_560.0, -50_608_005_120.0],
                    "owner_corporation_id": 1000045, "owner_faction_id": 500001
                }
            ]
        },
        {
            "solar_system_id": 30001407, "constellation_id": 20000206, "region_id": 10000016,
            "name": "Todaki", "security_status": 1.0, "radius": 2.228e12,
            "galactic_pos_ly": [-18.372, 10.050, 12.937],
            "planet_ids": [40089010, 40089015],
            "stations": [
                {
                    "station_id": 60004420, "station_type_id": 1529,
                    "name": "Todaki VI - Moon 1 - School of Applied Knowledge",
                    "position": [1_337_258_188_800.0, 62_467_399_680.0, -415_036_661_760.0],
                    "owner_corporation_id": 1000044, "owner_faction_id": 500001
                }
            ]
        },
        {
            "solar_system_id": 30001672, "constellation_id": 20000247, "region_id": 10000020,
            "name": "Emrayur", "security_status": 1.0, "radius": 1.318e12,
            "galactic_pos_ly": [-23.267, 7.165, -13.959],
            "planet_ids": [40106101, 40106105],
            "stations": [
                {
                    "station_id": 60007702, "station_type_id": 1931,
                    "name": "Emrayur III - Moon 1 - Royal Amarr Institute School",
                    "position": [-99_593_379_840.0, 16_226_181_120.0, -119_367_229_440.0],
                    "owner_corporation_id": 1000077, "owner_faction_id": 500003
                }
            ]
        },
        {
            "solar_system_id": 30002505, "constellation_id": 20000367, "region_id": 10000030,
            "name": "Hulm", "security_status": 1.0, "radius": 7.818e11,
            "galactic_pos_ly": [-9.179, 3.964, 0.322],
            "planet_ids": [40159201, 40159205],
            "stations": [
                {
                    "station_id": 60014779, "station_type_id": 2500,
                    "name": "Hulm VIII - Moon 2 - Republic University",
                    "position": [422_725_263_360.0, 33_719_377_920.0, -1_301_532_303_360.0],
                    "owner_corporation_id": 1000171, "owner_faction_id": 500002
                }
            ]
        },
        {
            "solar_system_id": 30002547, "constellation_id": 20000373, "region_id": 10000030,
            "name": "Ammold", "security_status": 1.0, "radius": 2.559e12,
            "galactic_pos_ly": [-9.021, 1.982, 4.802],
            "planet_ids": [40161801, 40161805],
            "stations": [
                {
                    "station_id": 60014749, "station_type_id": 2500,
                    "name": "Ammold V - Republic Military School",
                    "position": [-26_953_850_880.0, 1_041_408_000.0, -326_680_289_280.0],
                    "owner_corporation_id": 1000170, "owner_faction_id": 500002
                }
            ]
        },
        {
            "solar_system_id": 30002715, "constellation_id": 20000397, "region_id": 10000032,
            "name": "Bourynes", "security_status": 1.0, "radius": 1.003e12,
            "galactic_pos_ly": [-17.566, 4.701, 2.851],
            "planet_ids": [40172501, 40172505],
            "stations": [
                {
                    "station_id": 60011566, "station_type_id": 57,
                    "name": "Bourynes VII - Moon 2 - University of Caille",
                    "position": [1_064_538_808_320.0, 175_499_304_960.0, -716_995_092_480.0],
                    "owner_corporation_id": 1000115, "owner_faction_id": 500004
                }
            ]
        },
        {
            "solar_system_id": 30003410, "constellation_id": 20000497, "region_id": 10000042,
            "name": "Ryddinjorn", "security_status": 1.0, "radius": 1.333e12,
            "galactic_pos_ly": [-6.405, 4.505, 3.415],
            "planet_ids": [40216201, 40216205],
            "stations": [
                {
                    "station_id": 60014809, "station_type_id": 2500,
                    "name": "Ryddinjorn VI - Moon 2 - Pator Tech School",
                    "position": [444_369_346_560.0, 1_833_984_000.0, 1_093_726_986_240.0],
                    "owner_corporation_id": 1000172, "owner_faction_id": 500002
                }
            ]
        },
        {
            "solar_system_id": 30003489, "constellation_id": 20000509, "region_id": 10000043,
            "name": "Chaven", "security_status": 1.0, "radius": 1.738e12,
            "galactic_pos_ly": [-19.065, 6.343, -5.792],
            "planet_ids": [40221101, 40221105],
            "stations": [
                {
                    "station_id": 60014629, "station_type_id": 1931,
                    "name": "Chaven VIII - Moon 1 - Imperial Academy",
                    "position": [-2_316_436_316_160.0, 119_356_661_760.0, -926_632_304_640.0],
                    "owner_corporation_id": 1000166, "owner_faction_id": 500003
                }
            ]
        },
        {
            "solar_system_id": 30004971, "constellation_id": 20000727, "region_id": 10000064,
            "name": "Duripant", "security_status": 1.0, "radius": 7.253e11,
            "galactic_pos_ly": [-22.479, 5.513, 3.015],
            "planet_ids": [40314201, 40314205],
            "stations": [
                {
                    "station_id": 60014689, "station_type_id": 57,
                    "name": "Duripant VII - Moon 6 - Federal Navy Academy",
                    "position": [-373_473_239_040.0, 42_729_922_560.0, -1_415_498_342_400.0],
                    "owner_corporation_id": 1000168, "owner_faction_id": 500004
                }
            ]
        },
        {
            "solar_system_id": 30005042, "constellation_id": 20000737, "region_id": 10000065,
            "name": "Sehmy", "security_status": 1.0, "radius": 1.048e12,
            "galactic_pos_ly": [-27.619, 5.357, -11.098],
            "planet_ids": [40318801, 40318805],
            "stations": [
                {
                    "station_id": 60014599, "station_type_id": 1931,
                    "name": "Sehmy VIII - Moon 2 - Hedion University",
                    "position": [502_645_432_320.0, 45_862_379_520.0, 1_989_488_025_600.0],
                    "owner_corporation_id": 1000165, "owner_faction_id": 500003
                }
            ]
        },
        {
            "solar_system_id": 30005305, "constellation_id": 20000776, "region_id": 10000068,
            "name": "Cistuvaert", "security_status": 1.0, "radius": 3.226e12,
            "galactic_pos_ly": [-27.342, 5.110, 3.704],
            "planet_ids": [40335101, 40335105],
            "stations": [
                {
                    "station_id": 60014719, "station_type_id": 57,
                    "name": "Cistuvaert V - Moon 12 - Center for Advanced Studies School",
                    "position": [-740_232_929_280.0, 68_913_438_720.0, -1_058_469_765_120.0],
                    "owner_corporation_id": 1000169, "owner_faction_id": 500004
                }
            ]
        },
        {
            "solar_system_id": 30000144, "constellation_id": 20000020, "region_id": 10000002,
            "name": "Perimeter", "security_status": 0.9493, "radius": 3.2e12,
            "galactic_pos_ly": [-15.143, 6.862, 11.012],
            "planet_ids": [40009210, 40009215, 40009220],
            "stations": [
                {
                    "station_id": 60003766, "station_type_id": 1529,
                    "name": "Perimeter II - Moon 1 - Caldari Constructions Warehouse",
                    "position": [85_000_000_000.0, 12_000_000_000.0, -140_000_000_000.0],
                    "owner_corporation_id": 1000026, "owner_faction_id": 500001,
                    "has_clone_bay": False
                }
            ]
        },
        {
            "solar_system_id": 30002187, "constellation_id": 20000322, "region_id": 10000043,
            "name": "Amarr", "security_status": 1.0, "radius": 5.1e12,
            "galactic_pos_ly": [-21.642, 4.253, -6.091],
            "planet_ids": [40138801, 40138805, 40138810],
            "stations": [
                {
                    "station_id": 60008494, "station_type_id": 1926,
                    "name": "Amarr VIII (Oris) - Emperor Family Academy",
                    "position": [-210_000_000_000.0, 42_000_000_000.0, 95_000_000_000.0],
                    "owner_corporation_id": 1000084, "owner_faction_id": 500003
                }
            ]
        },
        {
            "solar_system_id": 30002659, "constellation_id": 20000389, "region_id": 10000032,
            "name": "Dodixie", "security_status": 0.8736, "radius": 4.0e12,
            "galactic_pos_ly": [-19.831, 5.628, 2.927],
            "planet_ids": [40168901, 40168905, 40168910],
            "stations": [
                {
                    "station_id": 60011866, "station_type_id": 1530,
                    "name": "Dodixie IX - Moon 20 - Federation Navy Assembly Plant",
                    "position": [110_000_000_000.0, -35_000_000_000.0, 290_000_000_000.0],
                    "owner_corporation_id": 1000120, "owner_faction_id": 500004
                }
            ]
        },
        {
            "solar_system_id": 30002510, "constellation_id": 20000368, "region_id": 10000030,
            "name": "Rens", "security_status": 0.8981, "radius": 3.8e12,
            "galactic_pos_ly": [-10.477, 4.263, -0.303],
            "planet_ids": [40159501, 40159505, 40159510],
            "stations": [
                {
                    "station_id": 60004588, "station_type_id": 2500,
                    "name": "Rens VI - Moon 8 - Brutor Tribe Treasury",
                    "position": [-95_000_000_000.0, 15_000_000_000.0, -180_000_000_000.0],
                    "owner_corporation_id": 1000049, "owner_faction_id": 500002
                }
            ]
        },
        {
            "solar_system_id": 30002813, "constellation_id": 20000413, "region_id": 10000033,
            "name": "Tama", "security_status": 0.281, "radius": 2.9e12,
            "galactic_pos_ly": [-20.473, 8.187, 11.911],
            "planet_ids": [40178401, 40178405, 40178410],
            "stations": []
        },
        {
            "solar_system_id": 30002741, "constellation_id": 20000401, "region_id": 10000033,
            "name": "Horkkisen", "security_status": 0.4384, "radius": 2.5e12,
            "galactic_pos_ly": [-13.535, 11.157, 16.175],
            "planet_ids": [40174101, 40174105],
            "stations": []
        },
        {
            "solar_system_id": 30002904, "constellation_id": 20000426, "region_id": 10000035,
            "name": "VFK-IV", "security_status": -0.45, "radius": 4.5e12,
            "galactic_pos_ly": [-26.182, 7.646, 30.120],
            "planet_ids": [40184101, 40184105],
            "stations": []
        },
        {
            "solar_system_id": 31000005, "constellation_id": 21000001, "region_id": 11000001,
            "name": "J105934", "security_status": -1.0, "radius": 3.1e12,
            "galactic_pos_ly": [0.0, 0.0, 0.0],
            "planet_ids": [40400001, 40400005],
            "stations": []
        },
        {
            "solar_system_id": 32000001, "constellation_id": 22000001, "region_id": 12000001,
            "name": "Abyssal Deadspace", "security_status": -1.0, "radius": 1.0e9,
            "galactic_pos_ly": [0.0, 0.0, 0.0],
            "planet_ids": [],
            "stations": []
        },
    ]

    jumps = [
        {"stargate_id": 50001248, "from_solar_system_id": 30000142, "to_solar_system_id": 30000144, "destination_stargate_id": 50001873, "position": [-161_369_743_360.0, -30_690_549_760.0, -1_107_962_183_680.0]},
        {"stargate_id": 50001873, "from_solar_system_id": 30000144, "to_solar_system_id": 30000142, "destination_stargate_id": 50001248, "position": [84_000_000_000.0, 12_000_000_000.0, -139_000_000_000.0]},
        {"stargate_id": 50001249, "from_solar_system_id": 30000142, "to_solar_system_id": 30000141, "destination_stargate_id": 50001252, "position": [-125_400_000_000.0, 14_200_000_000.0, 310_800_000_000.0]},
        {"stargate_id": 50001252, "from_solar_system_id": 30000141, "to_solar_system_id": 30000142, "destination_stargate_id": 50001249, "position": [64_000_000_000.0, -14_000_000_000.0, 179_000_000_000.0]},
        {"stargate_id": 50001250, "from_solar_system_id": 30000144, "to_solar_system_id": 30002813, "destination_stargate_id": 50001251, "position": [86_000_000_000.0, 12_000_000_000.0, -141_000_000_000.0]},
        {"stargate_id": 50001251, "from_solar_system_id": 30002813, "to_solar_system_id": 30000144, "destination_stargate_id": 50001250, "position": [10_000_000_000.0, 0.0, 10_000_000_000.0]},
    ]

    # 6. Clone Grades, Blueprints, Dbuffs, Fighter Abilities, Wrecks, Compressible, Type Materials, Dynamic Items, Space Components
    clone_grades = [
        {"grade_id": 1, "name": "Alpha", "max_skill_points": 5_000_000, "training_speed_multiplier": 0.5},
        {"grade_id": 2, "name": "Omega", "max_skill_points": 999_999_999, "training_speed_multiplier": 1.0},
    ]

    blueprints = [
        {
            "blueprint_type_id": 687,
            "product_type_id": 587,
            "product_quantity": 1,
            "manufacturing_time_sec": 6000,
            "research_me_time_sec": 2100,
            "research_te_time_sec": 2100,
            "copy_time_sec": 4800,
            "invention_time_sec": 36000,
            "invention_base_probability": 0.30,
            "materials": [
                {"type_id": 34, "quantity": 32_000},
                {"type_id": 35, "quantity": 6_000},
                {"type_id": 36, "quantity": 2_500},
                {"type_id": 37, "quantity": 500},
            ],
        },
        {
            "blueprint_type_id": 11372,
            "product_type_id": 11371,
            "product_quantity": 1,
            "manufacturing_time_sec": 12000,
            "research_me_time_sec": 4200,
            "research_te_time_sec": 4200,
            "copy_time_sec": 9600,
            "invention_time_sec": 0,
            "invention_base_probability": 0.0,
            "materials": [
                {"type_id": 587, "quantity": 1},
                {"type_id": 34, "quantity": 12_000},
                {"type_id": 38, "quantity": 400},
            ],
        },
    ]

    dbuff_collections = [
        {"dbuff_id": 10, "name": "Shield Harmonizing", "operation": "PostPercent", "aggregate_mode": "Maximum", "target_attribute_ids": [271, 272, 273, 274]},
        {"dbuff_id": 11, "name": "Active Shielding", "operation": "PostPercent", "aggregate_mode": "Maximum", "target_attribute_ids": [73]},
        {"dbuff_id": 12, "name": "Shield Extension", "operation": "PostPercent", "aggregate_mode": "Maximum", "target_attribute_ids": [263]},
    ]

    fighter_abilities = [
        {"ability_id": 1, "name": "Pulse Cannon", "cooldown_sec": 4.0, "damage_multiplier": 1.5, "charges": 0},
        {"ability_id": 2, "name": "Micromissile Swarm", "cooldown_sec": 30.0, "damage_multiplier": 4.5, "charges": 12},
        {"ability_id": 3, "name": "Microrowarpdrive", "cooldown_sec": 60.0, "damage_multiplier": 0.0, "charges": 0},
    ]

    wrecks = {
        "587": 26557,
        "11001": 26557,
    }

    compressible_types = {
        "1230": 28432,  # Veldspar -> Compressed Veldspar
        "1228": 28429,  # Scordite -> Compressed Scordite
    }

    # Hand-curated test-only type_materials seed data (pending Phase 1 D-1 real SDE importer)
    type_materials = {
        "18": [
            {"material_type_id": 34, "quantity": 175},
            {"material_type_id": 36, "quantity": 70},
        ],
        "438": [
            {"material_type_id": 34, "quantity": 135},
            {"material_type_id": 35, "quantity": 46},
            {"material_type_id": 36, "quantity": 28},
        ],
        "483": [
            {"material_type_id": 34, "quantity": 1324},
            {"material_type_id": 35, "quantity": 481},
            {"material_type_id": 36, "quantity": 119},
        ],
        "587": [
            {"material_type_id": 34, "quantity": 13333},
            {"material_type_id": 35, "quantity": 3333},
            {"material_type_id": 36, "quantity": 1333},
        ],
        "1228": [
            {"material_type_id": 34, "quantity": 150},
            {"material_type_id": 35, "quantity": 110},
        ],
        "1230": [
            {"material_type_id": 34, "quantity": 400},
        ],
        "28429": [
            {"material_type_id": 34, "quantity": 150},
            {"material_type_id": 35, "quantity": 110},
        ],
        "28432": [
            {"material_type_id": 34, "quantity": 400},
        ],
    }

    dynamic_item_attributes = [
        {
            "mutaplasmid_type_id": 47702,
            "applicable_type_ids": [438],
            "resulting_type_id": 47740,
            "attribute_ranges": {
                "6": [0.85, 1.20],   # capacitorNeed
                "20": [0.90, 1.15],  # speedFactor
                "30": [0.85, 1.25],  # power
                "50": [0.80, 1.30],  # cpu
            }
        }
    ]

    space_components_by_type = {
        "32882": ["ess", "bracket", "proximityLock"],
        "33474": ["tractorBeam", "autoTractorBeam", "cargoBay"],
        "35832": ["tether", "reinforce", "docking", "quantumCore", "structureControl"],
        "35840": ["tether", "reinforce", "quantumCore", "structureControl", "cynoBeacon"],
        "81140": ["skyhook", "reinforce", "reagentSilo"],
        "85230": ["mercenaryDen", "infomorphGeneration"],
    }

    mission_titles = [
        (1001, "Stop the Thief"),
        (1002, "Worlds Collide"),
        (1003, "The Blockade"),
        (1004, "Damsel in Distress"),
        (1005, "Gone Berserk"),
        (1006, "Recon (1 of 3)"),
        (1007, "Recon (2 of 3)"),
        (1008, "Recon (3 of 3)"),
        (1009, "Vengeance"),
        (1010, "Silence the Informant"),
        (1011, "Unauthorized Military Presence"),
        (1012, "Pirate Invasion"),
        (1013, "Attack of the Drones"),
        (1014, "Smuggler Interception"),
        (1015, "Duo of Death"),
        (1016, "The Guristas Spies"),
    ]
    missions = [
        {
            "mission_id": mid,
            "title": title,
            "level": 1 + ((mid - 1001) // 6),
            "mission_type": "Encounter",
            "agent_id": 3018924,
            "corporation_id": 1000035,
            "isk_reward": 150_000 + (mid - 1001) * 10_000,
            "time_bonus_isk": 120_000 + (mid - 1001) * 5_000,
            "lp_reward": 180 + (mid - 1001) * 12,
            "standing_gain_pct": round(1.25 + (mid - 1001) * 0.05, 2),
            "dungeon_id": 5001,
        }
        for mid, title in mission_titles
    ] + [
        {
            "mission_id": 2001,
            "title": "Materials for War Preparation",
            "level": 2,
            "mission_type": "StorylineCourier",
            "agent_id": 3018925,
            "corporation_id": 1000035,
            "isk_reward": 750_000,
            "time_bonus_isk": 500_000,
            "lp_reward": 800,
            "standing_gain_pct": 4.5,
            "dungeon_id": 0,
        }
    ]

    dungeons = [
        {
            "dungeon_id": 5001,
            "name": "Guristas Hideout",
            "archetype_id": 23,
            "rooms": [
                {
                    "room_id": 1,
                    "spawn_groups": [
                        {"npc_type_id": 11001, "count": 3, "bounty_isk": 100_000}
                    ],
                    "acceleration_gate_to_room": 2,
                },
                {
                    "room_id": 2,
                    "spawn_groups": [
                        {"npc_type_id": 11001, "count": 4, "bounty_isk": 100_000}
                    ],
                    "acceleration_gate_to_room": 0,
                },
            ],
        }
    ]

    epic_arcs = [
        {
            "epic_arc_id": 1,
            "name": "The Blood-Stained Stars (Sisters of EVE)",
            "chapters": 7,
            "missions_count": 52,
            "cooldown_days": 90,
        }
    ]

    node_graphs = [
        {
            "node_graph_id": 473,
            "name": "AIR New Player Experience (NPE)",
            "nodes": [
                {"node_id": 1, "node_type": "Start", "next_nodes": [2]},
                {"node_id": 2, "node_type": "ApproachAstero", "next_nodes": [3]},
                {"node_id": 3, "node_type": "DestroySeekers", "next_nodes": [4]},
                {"node_id": 4, "node_type": "DockAtAIRStation", "next_nodes": []},
            ],
        }
    ]

    career_goals = [
        {"goal_id": 1, "career_path": "Enforcer", "name": "Destroy 5 NPC Pirates", "target_progress": 5, "sp_reward": 10_000},
        {"goal_id": 2, "career_path": "Industrialist", "name": "Mine 1,000 Units of Ore", "target_progress": 1000, "sp_reward": 10_000},
        {"goal_id": 3, "career_path": "Explorer", "name": "Scan 1 Cosmic Signature", "target_progress": 1, "sp_reward": 10_000},
        {"goal_id": 4, "career_path": "SoldierOfFortune", "name": "Repair 500 Shield/Armor HP", "target_progress": 500, "sp_reward": 10_000},
    ]

    static_sqlite_count = 0
    fsdbinary_count = 0
    pickle_binary_count = 0
    if isinstance(static_meta, list):
        static_sqlite_count = sum(
            1 for e in static_meta
            if e.get("format") == "SQLite3" or str(e.get("res_path", "")).endswith(".static")
        )
        fsdbinary_count = sum(
            1 for e in static_meta
            if e.get("format") == "FSD Binary" or str(e.get("res_path", "")).endswith(".fsdbinary")
        )
        pickle_binary_count = sum(
            1 for e in static_meta
            if e.get("format") == "Pickle/Binary" or str(e.get("res_path", "")).endswith(".pickle")
        )
    elif isinstance(static_meta, dict):
        static_sqlite_count = len(static_meta.get("static_sqlite_databases", {}))
        fsdbinary_count = len(static_meta.get("fsdbinary_archives", {}))

    return {
        "schema_version": "V24.01-EvoMu-M1",
        "v24_staticdata_summary": {
            "static_sqlite_count": static_sqlite_count,
            "fsdbinary_count": fsdbinary_count,
            "pickle_binary_count": pickle_binary_count,
            "dogma_vm_operands": len(dogma_meta.get("operands", [])) if isinstance(dogma_meta.get("operands"), list) else 0,
            "dogma_effects_count": int(dogma_meta.get("effects_count", 0)) if isinstance(dogma_meta, dict) else 0,
        },
        "attributes": attributes,
        "effects": effects,
        "types": types,
        "races": races,
        "bloodlines": bloodlines,
        "ancestries": ancestries,
        "schools": schools,
        "solar_systems": solar_systems,
        "jumps": jumps,
        "clone_grades": clone_grades,
        "blueprints": blueprints,
        "dbuff_collections": dbuff_collections,
        "fighter_abilities": fighter_abilities,
        "wrecks": wrecks,
        "compressible_types": compressible_types,
        "type_materials": type_materials,
        "dynamic_item_attributes": dynamic_item_attributes,
        "space_components_by_type": space_components_by_type,
        "missions": missions,
        "dungeons": dungeons,
        "epic_arcs": epic_arcs,
        "node_graphs": node_graphs,
        "career_goals": career_goals,
    }


def main() -> int:
    seed = build_canonical_sde_seed()
    out_path = INDEX_DIR / "sde_seed.json"
    out_path.write_text(json.dumps(seed, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        f"[OK] Generated {out_path.relative_to(REPO_ROOT)}: "
        f"{len(seed['types'])} types, {len(seed['attributes'])} attributes, "
        f"{len(seed['effects'])} effects, {len(seed['solar_systems'])} solar systems, "
        f"{len(seed['jumps'])} jumps, {len(seed['blueprints'])} blueprints"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
