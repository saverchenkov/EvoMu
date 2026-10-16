#!/usr/bin/env python3
"""
extract_staticdata.py — V24.01 Static Data Ingestion & Seed Generator (Milestone M1).

Combines the indexed V24.01 `staticdata_catalog.json`, `dogma_effects.json`, and
`wiki_mechanics_catalog.json` to emit `docs/index/sde_seed.json`, which is loaded
by `crates/eve-sde` for deterministic data-driven mechanics across M1..M8.
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
        # Ships (Category 6) — Frigates, Corvettes, Industrial, Battleship, Capsule
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
                "1132": 400.0, "1137": 3.0
            }
        },
        {
            "type_id": 588, "group_id": 237, "category_id": 6, "name": "Reaper",
            "mass": 1_128_000.0, "volume": 28_600.0, "capacity": 115.0, "radius": 34.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_128_000.0, "9": 225.0, "11": 30.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 315.0, "38": 115.0, "48": 105.0, "55": 95_000.0, "70": 3.65,
                "263": 250.0, "265": 225.0, "479": 625_000.0, "482": 180.0, "552": 41.0, "564": 550.0, "600": 5.0
            }
        },
        {
            "type_id": 596, "group_id": 237, "category_id": 6, "name": "Impairor",
            "mass": 1_240_000.0, "volume": 27_400.0, "capacity": 120.0, "radius": 35.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_240_000.0, "9": 250.0, "11": 35.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 295.0, "38": 120.0, "48": 95.0, "55": 100_000.0, "70": 3.7,
                "263": 200.0, "265": 275.0, "479": 625_000.0, "482": 210.0, "552": 42.0, "564": 540.0, "600": 5.0
            }
        },
        {
            "type_id": 601, "group_id": 237, "category_id": 6, "name": "Ibis",
            "mass": 1_197_000.0, "volume": 27_600.0, "capacity": 125.0, "radius": 35.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_197_000.0, "9": 220.0, "11": 28.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 280.0, "38": 125.0, "48": 120.0, "55": 95_000.0, "70": 3.8,
                "263": 280.0, "265": 200.0, "479": 625_000.0, "482": 195.0, "552": 44.0, "564": 520.0, "600": 5.0
            }
        },
        {
            "type_id": 606, "group_id": 237, "category_id": 6, "name": "Velator",
            "mass": 1_175_000.0, "volume": 28_100.0, "capacity": 135.0, "radius": 36.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 1_175_000.0, "9": 265.0, "11": 32.0, "12": 2.0, "13": 2.0, "14": 2.0,
                "37": 300.0, "38": 135.0, "48": 110.0, "55": 95_000.0, "70": 3.7,
                "263": 225.0, "265": 240.0, "283": 10.0, "479": 625_000.0, "482": 190.0, "552": 43.0, "564": 540.0, "600": 5.0, "1271": 10.0
            }
        },
        {
            "type_id": 597, "group_id": 25, "category_id": 6, "name": "Punisher",
            "mass": 1_195_000.0, "volume": 24_500.0, "capacity": 135.0, "radius": 35.0,
            "base_price": 250_000.0, "published": True,
            "default_attributes": {
                "4": 1_195_000.0, "9": 400.0, "11": 60.0, "12": 5.0, "13": 2.0, "14": 4.0,
                "37": 325.0, "38": 135.0, "48": 110.0, "55": 140_000.0, "70": 3.6,
                "263": 350.0, "265": 650.0, "479": 625_000.0, "482": 345.0, "552": 36.0, "564": 620.0, "600": 5.0
            }
        },
        {
            "type_id": 603, "group_id": 25, "category_id": 6, "name": "Merlin",
            "mass": 1_140_000.0, "volume": 26_000.0, "capacity": 145.0, "radius": 36.0,
            "base_price": 240_000.0, "published": True,
            "default_attributes": {
                "4": 1_140_000.0, "9": 375.0, "11": 43.0, "12": 3.0, "13": 4.0, "14": 3.0,
                "37": 310.0, "38": 145.0, "48": 155.0, "55": 135_000.0, "70": 3.7,
                "263": 650.0, "265": 350.0, "479": 625_000.0, "482": 290.0, "552": 39.0, "564": 600.0, "600": 5.0
            }
        },
        {
            "type_id": 594, "group_id": 25, "category_id": 6, "name": "Incursus",
            "mass": 1_120_000.0, "volume": 25_500.0, "capacity": 140.0, "radius": 38.0,
            "base_price": 245_000.0, "published": True,
            "default_attributes": {
                "4": 1_120_000.0, "9": 450.0, "11": 48.0, "12": 4.0, "13": 3.0, "14": 3.0,
                "37": 340.0, "38": 140.0, "48": 135.0, "55": 130_000.0, "70": 3.55,
                "263": 400.0, "265": 500.0, "283": 5.0, "479": 625_000.0, "482": 300.0, "552": 37.0, "564": 640.0, "600": 5.0, "1271": 5.0
            }
        },
        {
            "type_id": 32880, "group_id": 1283, "category_id": 6, "name": "Venture",
            "mass": 1_200_000.0, "volume": 29_200.0, "capacity": 5000.0, "radius": 40.0,
            "base_price": 300_000.0, "published": True,
            "default_attributes": {
                "4": 1_200_000.0, "9": 400.0, "11": 35.0, "12": 1.0, "13": 3.0, "14": 3.0,
                "37": 335.0, "38": 5000.0, "48": 240.0, "55": 125_000.0, "70": 3.6,
                "102": 2.0, "105": -2.0, "263": 550.0, "265": 350.0, "283": 10.0,
                "479": 625_000.0, "482": 250.0, "552": 40.0, "564": 700.0, "600": 5.0, "1271": 10.0
            }
        },
        {
            "type_id": 638, "group_id": 27, "category_id": 6, "name": "Raven",
            "mass": 99_300_000.0, "volume": 486_000.0, "capacity": 690.0, "radius": 320.0,
            "base_price": 210_000_000.0, "published": True,
            "default_attributes": {
                "4": 99_300_000.0, "9": 6600.0, "11": 10_500.0, "12": 5.0, "13": 7.0, "14": 7.0,
                "37": 115.0, "38": 690.0, "48": 750.0, "55": 1_000_000.0, "70": 0.125,
                "101": 6.0, "263": 8500.0, "265": 6000.0, "283": 75.0,
                "479": 2_500_000.0, "482": 5500.0, "552": 415.0, "564": 115.0, "600": 3.0, "1271": 50.0
            }
        },
        {
            "type_id": 670, "group_id": 29, "category_id": 6, "name": "Capsule",
            "mass": 16_000.0, "volume": 1000.0, "capacity": 0.0, "radius": 18.0,
            "base_price": 1.0, "published": True,
            "default_attributes": {
                "4": 16_000.0, "9": 350.0, "37": 210.0, "70": 0.06, "263": 250.0, "265": 250.0,
                "482": 100.0, "552": 25.0, "600": 3.0
            }
        },
        # Modules (Category 7)
        {
            "type_id": 438, "group_id": 46, "category_id": 7, "name": "1MN Afterburner II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 500_000.0, "published": True,
            "default_attributes": {
                "4": 1.0, "6": 20.0, "20": 135.0, "30": 15.0, "50": 25.0, "73": 7500.0, "796": 500_000.0, "1196": 1.4
            }
        },
        {
            "type_id": 1248, "group_id": 762, "category_id": 7, "name": "Overdrive Injector System II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 350_000.0, "published": True,
            "default_attributes": {"20": 12.5, "30": 1.0, "50": 15.0}
        },
        {
            "type_id": 2889, "group_id": 55, "category_id": 7, "name": "200mm AutoCannon II",
            "mass": 1.0, "volume": 5.0, "capacity": 5.0, "radius": 1.0,
            "base_price": 600_000.0, "published": True,
            "default_attributes": {
                "30": 5.0, "50": 10.0, "51": 3125.0, "54": 1500.0, "64": 2.4, "158": 8400.0, "160": 240.0, "1196": 1.2
            }
        },
        {
            "type_id": 448, "group_id": 52, "category_id": 7, "name": "Warp Scrambler II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 800_000.0, "published": True,
            "default_attributes": {
                "6": 6.0, "30": 1.0, "50": 30.0, "54": 9000.0, "73": 5000.0, "105": 2.0
            }
        },
        {
            "type_id": 2048, "group_id": 60, "category_id": 7, "name": "Damage Control II",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 550_000.0, "published": True,
            "default_attributes": {"30": 1.0, "50": 30.0}
        },
        {
            "type_id": 31788, "group_id": 774, "category_id": 7, "name": "Small Core Defense Field Extender I",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 900_000.0, "published": True,
            "default_attributes": {"68": 15.0, "1153": 50.0}
        },
        {
            "type_id": 483, "group_id": 54, "category_id": 7, "name": "Miner I",
            "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0,
            "base_price": 50_000.0, "published": True,
            "default_attributes": {"6": 10.0, "30": 2.0, "50": 30.0, "54": 10_000.0, "73": 60_000.0}
        },
        {
            "type_id": 54800, "group_id": 4060, "category_id": 7, "name": "Small Vorton Projector I",
            "mass": 1.0, "volume": 5.0, "capacity": 5.0, "radius": 1.0,
            "base_price": 2_500_000.0, "published": True,
            "default_attributes": {"30": 8.0, "50": 20.0, "51": 5000.0, "54": 18_000.0, "114": 40.0, "117": 40.0}
        },
        {
            "type_id": 47936, "group_id": 1986, "category_id": 7, "name": "Heavy Entropic Disintegrator II",
            "mass": 1.0, "volume": 20.0, "capacity": 10.0, "radius": 1.0,
            "base_price": 12_000_000.0, "published": True,
            "default_attributes": {"30": 900.0, "50": 55.0, "51": 3500.0, "54": 48_000.0, "64": 2.8, "116": 65.0, "118": 85.0}
        },
        {
            "type_id": 84010, "group_id": 4500, "category_id": 7, "name": "Small Breacher Pod Launcher I",
            "mass": 1.0, "volume": 5.0, "capacity": 5.0, "radius": 1.0,
            "base_price": 4_000_000.0, "published": True,
            "default_attributes": {"30": 6.0, "50": 18.0, "51": 8000.0, "54": 25_000.0}
        },
        # Minerals, Ores, Charges, Injectors, Quantum Cores & Clone Soldier Tags
        {"type_id": 34, "group_id": 18, "category_id": 4, "name": "Tritanium", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5.0, "published": True, "default_attributes": {}},
        {"type_id": 35, "group_id": 18, "category_id": 4, "name": "Pyerite", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 12.0, "published": True, "default_attributes": {}},
        {"type_id": 36, "group_id": 18, "category_id": 4, "name": "Mexallon", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 75.0, "published": True, "default_attributes": {}},
        {"type_id": 37, "group_id": 18, "category_id": 4, "name": "Isogen", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 150.0, "published": True, "default_attributes": {}},
        {"type_id": 38, "group_id": 18, "category_id": 4, "name": "Nocxium", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 600.0, "published": True, "default_attributes": {}},
        {"type_id": 39, "group_id": 18, "category_id": 4, "name": "Zydrine", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 1200.0, "published": True, "default_attributes": {}},
        {"type_id": 40, "group_id": 18, "category_id": 4, "name": "Megacyte", "mass": 1.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 2400.0, "published": True, "default_attributes": {}},
        {"type_id": 1230, "group_id": 462, "category_id": 25, "name": "Veldspar", "mass": 1.0, "volume": 0.1, "capacity": 0.0, "radius": 1.0, "base_price": 15.0, "published": True, "default_attributes": {}},
        {"type_id": 28432, "group_id": 462, "category_id": 25, "name": "Compressed Veldspar", "mass": 1.0, "volume": 0.001, "capacity": 0.0, "radius": 1.0, "base_price": 1500.0, "published": True, "default_attributes": {}},
        {"type_id": 1228, "group_id": 460, "category_id": 25, "name": "Scordite", "mass": 1.0, "volume": 0.15, "capacity": 0.0, "radius": 1.0, "base_price": 22.0, "published": True, "default_attributes": {}},
        {"type_id": 28429, "group_id": 460, "category_id": 25, "name": "Compressed Scordite", "mass": 1.0, "volume": 0.0015, "capacity": 0.0, "radius": 1.0, "base_price": 2200.0, "published": True, "default_attributes": {}},
        {
            "type_id": 22989, "group_id": 83, "category_id": 8, "name": "Republic Fleet Phased Plasma S",
            "mass": 1.0, "volume": 0.0025, "capacity": 0.0, "radius": 1.0, "base_price": 800.0, "published": True,
            "default_attributes": {"116": 2.0, "117": 2.0, "118": 10.0}
        },
        {"type_id": 40520, "group_id": 1739, "category_id": 5, "name": "Large Skill Injector", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 850_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 56201, "group_id": 4088, "category_id": 17, "name": "Astrahus Upwell Quantum Core", "mass": 5000.0, "volume": 100.0, "capacity": 0.0, "radius": 1.0, "base_price": 600_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35661, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Trainer Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35662, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Recruiter Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 15_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35663, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Transporter Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 25_000_000.0, "published": True, "default_attributes": {}},
        {"type_id": 35664, "group_id": 1206, "category_id": 17, "name": "Clone Soldier Negotiator Tag", "mass": 0.1, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 45_000_000.0, "published": True, "default_attributes": {}},
        # Skills (Category 16)
        {"type_id": 3327, "group_id": 257, "category_id": 16, "name": "Spaceship Command", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3328, "group_id": 257, "category_id": 16, "name": "Minmatar Frigate", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 40_000.0, "published": True, "default_attributes": {"275": 2.0}},
        {"type_id": 3449, "group_id": 275, "category_id": 16, "name": "Navigation", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3452, "group_id": 275, "category_id": 16, "name": "Acceleration Control", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 120_000.0, "published": True, "default_attributes": {"275": 4.0}},
        {"type_id": 3300, "group_id": 255, "category_id": 16, "name": "Gunnery", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3302, "group_id": 255, "category_id": 16, "name": "Small Projectile Turret", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3380, "group_id": 268, "category_id": 16, "name": "Industry", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3385, "group_id": 268, "category_id": 16, "name": "Reprocessing", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 30_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3443, "group_id": 274, "category_id": 16, "name": "Trade", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 20_000.0, "published": True, "default_attributes": {"275": 1.0}},
        {"type_id": 3446, "group_id": 274, "category_id": 16, "name": "Broker Relations", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {"275": 2.0}},
        {"type_id": 16622, "group_id": 274, "category_id": 16, "name": "Accounting", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 5_000_000.0, "published": True, "default_attributes": {"275": 3.0}},
        {"type_id": 28164, "group_id": 269, "category_id": 16, "name": "Thermodynamics", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 3_500_000.0, "published": True, "default_attributes": {"275": 3.0}},
        # Blueprints (Category 9)
        {"type_id": 687, "group_id": 105, "category_id": 9, "name": "Rifter Blueprint", "mass": 0.0, "volume": 0.01, "capacity": 0.0, "radius": 1.0, "base_price": 2_500_000.0, "published": True, "default_attributes": {}},
        # Mutaplasmids & Abyssal Dynamic Items
        {"type_id": 47702, "group_id": 1979, "category_id": 5, "name": "Decayed 1MN Afterburner Mutaplasmid", "mass": 0.1, "volume": 1.0, "capacity": 0.0, "radius": 1.0, "base_price": 100_000.0, "published": True, "default_attributes": {}},
        {"type_id": 47740, "group_id": 46, "category_id": 7, "name": "Abyssal 1MN Afterburner", "mass": 1.0, "volume": 5.0, "capacity": 0.0, "radius": 1.0, "base_price": 1_500_000.0, "published": True, "default_attributes": {"4": 1.0, "6": 20.0, "20": 135.0, "30": 15.0, "50": 25.0, "73": 7500.0, "796": 500_000.0}},
        # Structures & Deployables (Category 65 & 22)
        {"type_id": 35832, "group_id": 1657, "category_id": 65, "name": "Astrahus", "mass": 500_000_000.0, "volume": 8000.0, "capacity": 100_000.0, "radius": 10_000.0, "base_price": 800_000_000.0, "published": True, "default_attributes": {"9": 1_800_000.0, "263": 1_800_000.0, "265": 1_800_000.0}},
        {"type_id": 33474, "group_id": 1250, "category_id": 22, "name": "Mobile Tractor Unit", "mass": 5000.0, "volume": 100.0, "capacity": 27_000.0, "radius": 25.0, "base_price": 8_000_000.0, "published": True, "default_attributes": {"9": 25_000.0, "263": 25_000.0}},
        {"type_id": 32882, "group_id": 1246, "category_id": 22, "name": "Encounter Surveillance System", "mass": 100_000.0, "volume": 500.0, "capacity": 0.0, "radius": 120.0, "base_price": 50_000_000.0, "published": True, "default_attributes": {"9": 500_000.0}},
        {"type_id": 81140, "group_id": 4700, "category_id": 65, "name": "Orbital Skyhook", "mass": 200_000_000.0, "volume": 10_000.0, "capacity": 500_000.0, "radius": 3500.0, "base_price": 1_200_000_000.0, "published": True, "default_attributes": {"9": 2_500_000.0}},
        {"type_id": 85230, "group_id": 4800, "category_id": 22, "name": "Mercenary Den", "mass": 50_000.0, "volume": 100.0, "capacity": 10_000.0, "radius": 150.0, "base_price": 75_000_000.0, "published": True, "default_attributes": {"9": 150_000.0}},
        # NPC Entities & Wrecks
        {"type_id": 11001, "group_id": 555, "category_id": 11, "name": "Guristas Pithi Invader", "mass": 1_000_000.0, "volume": 25_000.0, "capacity": 100.0, "radius": 35.0, "base_price": 15_000.0, "published": False, "default_attributes": {"9": 200.0, "37": 310.0, "263": 300.0, "265": 150.0, "552": 38.0}},
        {"type_id": 11003, "group_id": 301, "category_id": 11, "name": "CONCORD Police Commander", "mass": 50_000_000.0, "volume": 150_000.0, "capacity": 500.0, "radius": 150.0, "base_price": 0.0, "published": False, "default_attributes": {"9": 100_000.0, "37": 1500.0, "263": 100_000.0, "265": 100_000.0, "552": 100.0}},
        {"type_id": 26557, "group_id": 186, "category_id": 2, "name": "Small Minmatar Frigate Wreck", "mass": 500_000.0, "volume": 5000.0, "capacity": 1000.0, "radius": 30.0, "base_price": 0.0, "published": False, "default_attributes": {"9": 500.0}},
    ]

    # 4. Races, Bloodlines, Ancestries, Schools
    races = [
        {"race_id": 1, "name": "Caldari", "corvette_type_id": 601},
        {"race_id": 2, "name": "Minmatar", "corvette_type_id": 588},
        {"race_id": 4, "name": "Amarr", "corvette_type_id": 596},
        {"race_id": 8, "name": "Gallente", "corvette_type_id": 606},
    ]
    bloodlines = [
        {"bloodline_id": 1, "race_id": 1, "name": "Deteis", "corporation_id": 1000006},
        {"bloodline_id": 3, "race_id": 2, "name": "Sebiestor", "corporation_id": 1000125},
        {"bloodline_id": 5, "race_id": 4, "name": "Amarr", "corporation_id": 1000077},
        {"bloodline_id": 7, "race_id": 8, "name": "Gallente", "corporation_id": 1000115},
    ]
    ancestries = [
        {"ancestry_id": 1, "bloodline_id": 1, "name": "Scientists"},
        {"ancestry_id": 13, "bloodline_id": 3, "name": "Tinkers"},
        {"ancestry_id": 25, "bloodline_id": 5, "name": "Liberal Holders"},
        {"ancestry_id": 31, "bloodline_id": 7, "name": "Activists"},
    ]
    schools = [
        {"school_id": 11, "race_id": 1, "corporation_id": 1000044, "station_id": 60003760, "solar_system_id": 30000142, "name": "School of Applied Knowledge"},
        {"school_id": 17, "race_id": 2, "corporation_id": 1000170, "station_id": 60004588, "solar_system_id": 30002510, "name": "Republic University"},
        {"school_id": 19, "race_id": 4, "corporation_id": 1000165, "station_id": 60008494, "solar_system_id": 30002187, "name": "Hedion University"},
        {"school_id": 21, "race_id": 8, "corporation_id": 1000168, "station_id": 60011866, "solar_system_id": 30002659, "name": "University of Caille"},
    ]

    # 5. Solar Systems, Jumps & Content
    solar_systems = [
        {
            "solar_system_id": 30000142, "constellation_id": 20000020, "region_id": 10000002,
            "name": "Jita", "security_status": 0.9459, "radius": 4.389e12,
            "stations": [
                {
                    "station_id": 60003760, "station_type_id": 1529,
                    "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
                    "position": [-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0]
                }
            ]
        },
        {
            "solar_system_id": 30000144, "constellation_id": 20000020, "region_id": 10000002,
            "name": "Perimeter", "security_status": 0.9493, "radius": 3.2e12,
            "stations": [
                {
                    "station_id": 60003766, "station_type_id": 1529,
                    "name": "Perimeter II - Moon 1 - Caldari Constructions Warehouse",
                    "position": [85_000_000_000.0, 12_000_000_000.0, -140_000_000_000.0]
                }
            ]
        },
        {
            "solar_system_id": 30002187, "constellation_id": 20000322, "region_id": 10000043,
            "name": "Amarr", "security_status": 1.0, "radius": 5.1e12,
            "stations": [
                {
                    "station_id": 60008494, "station_type_id": 1926,
                    "name": "Amarr VIII (Oris) - Emperor Family Academy",
                    "position": [-210_000_000_000.0, 42_000_000_000.0, 95_000_000_000.0]
                }
            ]
        },
        {
            "solar_system_id": 30002659, "constellation_id": 20000389, "region_id": 10000032,
            "name": "Dodixie", "security_status": 0.8736, "radius": 4.0e12,
            "stations": [
                {
                    "station_id": 60011866, "station_type_id": 1530,
                    "name": "Dodixie IX - Moon 20 - Federation Navy Assembly Plant",
                    "position": [110_000_000_000.0, -35_000_000_000.0, 290_000_000_000.0]
                }
            ]
        },
        {
            "solar_system_id": 30002510, "constellation_id": 20000368, "region_id": 10000030,
            "name": "Rens", "security_status": 0.8981, "radius": 3.8e12,
            "stations": [
                {
                    "station_id": 60004588, "station_type_id": 2500,
                    "name": "Rens VI - Moon 8 - Brutor Tribe Treasury",
                    "position": [-95_000_000_000.0, 15_000_000_000.0, -180_000_000_000.0]
                }
            ]
        },
        {
            "solar_system_id": 30002813, "constellation_id": 20000413, "region_id": 10000033,
            "name": "Tama", "security_status": 0.281, "radius": 2.9e12,
            "stations": []
        },
        {
            "solar_system_id": 30002904, "constellation_id": 20000426, "region_id": 10000035,
            "name": "VFK-IV", "security_status": -0.45, "radius": 4.5e12,
            "stations": []
        },
        {
            "solar_system_id": 31000005, "constellation_id": 21000001, "region_id": 11000001,
            "name": "J105934", "security_status": -1.0, "radius": 3.1e12,
            "stations": []
        },
        {
            "solar_system_id": 32000001, "constellation_id": 22000001, "region_id": 12000001,
            "name": "Abyssal Deadspace", "security_status": -1.0, "radius": 1.0e9,
            "stations": []
        },
    ]

    jumps = [
        {"stargate_id": 50001248, "from_solar_system_id": 30000142, "to_solar_system_id": 30000144, "destination_stargate_id": 50001249, "position": [-107_000_000_000.0, -18_000_000_000.0, 435_000_000_000.0]},
        {"stargate_id": 50001249, "from_solar_system_id": 30000144, "to_solar_system_id": 30000142, "destination_stargate_id": 50001248, "position": [84_000_000_000.0, 12_000_000_000.0, -139_000_000_000.0]},
        {"stargate_id": 50001250, "from_solar_system_id": 30000144, "to_solar_system_id": 30002813, "destination_stargate_id": 50001251, "position": [86_000_000_000.0, 12_000_000_000.0, -141_000_000_000.0]},
        {"stargate_id": 50001251, "from_solar_system_id": 30002813, "to_solar_system_id": 30000144, "destination_stargate_id": 50001250, "position": [10_000_000_000.0, 0.0, 10_000_000_000.0]},
    ]

    # 6. Clone Grades, Blueprints, Dbuffs, Fighter Abilities, Wrecks, Compressible, Dynamic Items, Space Components
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
        }
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
        "81140": ["skyhook", "reinforce", "reagentSilo"],
        "85230": ["mercenaryDen", "infomorphGeneration"],
    }

    missions = [
        {
            "mission_id": 1001,
            "title": "Stop the Thief",
            "level": 1,
            "mission_type": "Encounter",
            "agent_id": 3018924,
            "corporation_id": 1000035,
            "isk_reward": 150_000,
            "time_bonus_isk": 120_000,
            "lp_reward": 180,
            "standing_gain_pct": 1.25,
            "dungeon_id": 5001,
        },
        {
            "mission_id": 1002,
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
        },
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
                        {"npc_type_id": 11001, "count": 3, "bounty_isk": 15_000}
                    ],
                    "acceleration_gate_to_room": 2,
                },
                {
                    "room_id": 2,
                    "spawn_groups": [
                        {"npc_type_id": 11001, "count": 4, "bounty_isk": 20_000}
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

    # Optional live SharedCache SQLite3 (.static) ingestion when local EVE installation is present
    shared_cache = Path(os.path.expanduser("~/Library/Application Support/CCP/EVE/SharedCache/ResFiles"))
    if shared_cache.is_dir() and isinstance(static_meta, list):
        import sqlite3
        existing_type_ids = {t["type_id"] for t in types}
        for entry in static_meta:
            if str(entry.get("res_path", "")).endswith("types.static"):
                db_file = shared_cache / str(entry.get("cdn_relative_path", ""))
                if db_file.is_file():
                    try:
                        conn = sqlite3.connect(f"file:{db_file}?mode=ro", uri=True)
                        cur = conn.cursor()
                        for row in cur.execute("SELECT typeID, data FROM types LIMIT 500"):
                            tid = int(row[0])
                            if tid not in existing_type_ids:
                                payload = json.loads(row[1]) if isinstance(row[1], str) else {}
                                existing_type_ids.add(tid)
                                types.append({
                                    "type_id": tid,
                                    "group_id": int(payload.get("groupID", 0)),
                                    "category_id": int(payload.get("categoryID", 0)),
                                    "name": str(payload.get("typeNameID", f"Type_{tid}")),
                                    "mass": float(payload.get("mass", 1.0)),
                                    "volume": float(payload.get("volume", 1.0)),
                                    "capacity": float(payload.get("capacity", 0.0)),
                                    "radius": float(payload.get("radius", 1.0)),
                                    "base_price": float(payload.get("basePrice", 0.0)),
                                    "published": bool(payload.get("published", False)),
                                    "default_attributes": {},
                                })
                        conn.close()
                    except Exception:
                        pass

    return {
        "schema_version": "V24.01-EvoMu-M1",
        "v24_staticdata_summary": {
            "static_sqlite_count": static_sqlite_count,
            "fsdbinary_count": fsdbinary_count,
            "pickle_binary_count": pickle_binary_count,
            "dogma_vm_operands": len(dogma_meta.get("operands", [])) if isinstance(dogma_meta, dict) else 0,
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
