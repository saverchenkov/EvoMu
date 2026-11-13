//! Module effect ID resolution helpers for bound dogma objects.

/// Fallback SDE effect IDs for superweapons / Doomsdays when synthetic tracker names
/// (e.g. `"superWeapon"`, `"superWeaponLanceWarmup"`) or stripped test SDE types lack
/// `default_effect_id` (`#1019`).
pub(crate) fn fallback_superweapon_effect_id(
    type_id: u32,
    effect_name: Option<&str>,
) -> Option<i64> {
    match type_id {
        24550 => return Some(4489),                         // superWeaponAmarr
        24552 => return Some(4490),                         // superWeaponCaldari
        24554 => return Some(4491),                         // superWeaponGallente
        23674 => return Some(4492),                         // superWeaponMinmatar
        40633 => return Some(6473),                         // doomsdayConeDOT
        40634 => return Some(6474),                         // doomsdayHOG
        37474 | 40632 | 41442..=41444 => return Some(6201), // doomsdaySlash
        40631 | 41439..=41441 => return Some(6472),         // doomsdayBeamDOT
        77398..=77401 => return Some(11691),                // debuffLance
        _ => {}
    }
    match effect_name.unwrap_or("") {
        "superWeapon" | "superWeaponImmobilize" | "superWeaponAmarr" => Some(4489),
        "superWeaponCaldari" => Some(4490),
        "superWeaponGallente" => Some(4491),
        "superWeaponMinmatar" => Some(4492),
        "superWeaponCone" | "doomsdayConeDOT" => Some(6473),
        "superWeaponSpatial" | "doomsdayHOG" => Some(6474),
        "superWeaponMultiPoint" | "doomsdaySlash" => Some(6201),
        "superWeaponLanceWarmup" | "superWeaponLanceBeam" | "doomsdayBeamDOT" => Some(6472),
        "debuffLance" | "doomsdayAOEBalance" => Some(11691),
        _ => None,
    }
}

/// Helper to determine the effect ID for a module given its type ID and optional effect name.
pub fn effect_id_for_module_sde(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
    effect_name: Option<&str>,
) -> Option<i64> {
    let ty = sde.types.get(&type_id);
    if let Some(name) = effect_name {
        let in_type = ty.and_then(|t| {
            t.effect_ids
                .iter()
                .find(|eid| sde.effects.get(eid).is_some_and(|e| e.name == name))
                .copied()
        });
        if let Some(eid) = in_type {
            return Some(eid as i64);
        }
        if let Some(e) = sde.effects.values().find(|e| e.name == name) {
            return Some(e.effect_id as i64);
        }
    }
    ty.and_then(|t| t.default_effect_id)
        .map(|e| e as i64)
        .or_else(|| fallback_superweapon_effect_id(type_id, effect_name))
}

/// Resolves the `OnSpecialFX` `(guid, is_offensive)` tuple for an activated/deactivated space module
/// (GH `#804`, `eve/client/script/parklife/fxSequencer.pyj:344`).
/// Returns `None` for mining modules (which emit their own `OnSpecialFX` in `mining_cycle.rs`) and
/// passive modules without a visual space effect.
pub fn special_fx_guid_for_module_sde(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
    effect_id: i64,
    effect_name: Option<&str>,
) -> Option<(String, bool)> {
    let ty = sde.types.get(&type_id);
    let group_id = ty.map(|t| t.group_id).unwrap_or(0);
    let eff_name = effect_name.unwrap_or("");
    if matches!(group_id, 54 | 464 | 483 | 737 | 2004 | 4138)
        || matches!(effect_id, 17 | 67 | 2726)
        || matches!(eff_name, "miningLaser" | "miningClouds" | "mining")
    {
        return None;
    }
    if group_id == 330 || eff_name.to_ascii_lowercase().contains("cloak") {
        return Some(("effects.Cloak".to_string(), false));
    }
    let name_lower = ty
        .map(|t| t.name.to_ascii_lowercase())
        .unwrap_or_default();
    if group_id == 74
        || eff_name == "hybridFired"
        || (group_id == 53
            && (name_lower.contains("blaster") || name_lower.contains("railgun")))
    {
        return Some(("effects.HybridFired".to_string(), true));
    }
    if group_id == 55 || eff_name == "projectileFired" {
        return Some(("effects.ProjectileFired".to_string(), true));
    }
    if group_id == 53 || eff_name == "targetAttack" {
        return Some(("effects.Laser".to_string(), true));
    }
    let resolved_eid = if effect_id > 0 {
        u16::try_from(effect_id).ok()
    } else {
        ty.and_then(|t| t.default_effect_id)
    };
    if let Some(eid) = resolved_eid {
        if let Some(eff) = sde.effects.get(&eid) {
            if let Some(ref raw_guid) = eff.guid {
                if let Some(g) = raw_guid
                    .split(',')
                    .next()
                    .map(str::trim)
                    .filter(|s| s.starts_with("effects."))
                {
                    return Some((g.to_string(), eff.is_offensive));
                }
            }
        }
    }
    match group_id {
        65 => Some(("effects.ModifyTargetSpeed".to_string(), true)),
        52 => Some(("effects.WarpScramble".to_string(), true)),
        201 | 208 | 291 | 842 => Some(("effects.ElectronicAttributeModifyTarget".to_string(), true)),
        379 => Some(("effects.TargetPaint".to_string(), true)),
        71 => Some(("effects.EnergyDestabilization".to_string(), true)),
        68 => Some(("effects.EnergyVampire".to_string(), true)),
        46 => {
            let is_mwd = eff_name == "moduleBonusMicrowarpdrive"
                || ty.is_some_and(|t| t.effect_ids.contains(&6730))
                || name_lower.contains("microwarp");
            Some((
                if is_mwd {
                    "effects.MicroWarpDrive".to_string()
                } else {
                    "effects.Afterburner".to_string()
                },
                false,
            ))
        }
        40 | 1156 => Some(("effects.ShieldBoosting".to_string(), false)),
        62 | 1199 => Some(("effects.ArmorRepair".to_string(), false)),
        506..=511 | 524 | 1245 => Some(("effects.MissileDeployment".to_string(), true)),
        _ => match eff_name {
            "remoteWebifierFalloff" | "modifyTargetSpeed2" | "decreaseTargetSpeed" => {
                Some(("effects.ModifyTargetSpeed".to_string(), true))
            }
            "warpScramble"
            | "warpScrambleForEntity"
            | "warpScrambleBlockMWDWithNPCEffect" => {
                Some(("effects.WarpScramble".to_string(), true))
            }
            "remoteSensorDampFalloff"
            | "remoteECMFalloff"
            | "structureModuleEffectRemoteSensorDampener"
            | "remoteWeaponDisruptFalloff" => {
                Some(("effects.ElectronicAttributeModifyTarget".to_string(), true))
            }
            "ewTargetPaint" | "remoteTargetPaintFalloff" => {
                Some(("effects.TargetPaint".to_string(), true))
            }
            "energyDestabilizationNew" | "energyNeutralizerFalloff" => {
                Some(("effects.EnergyDestabilization".to_string(), true))
            }
            "energyVampire" | "energyNosferatuFalloff" => {
                Some(("effects.EnergyVampire".to_string(), true))
            }
            "moduleBonusMicrowarpdrive" => Some(("effects.MicroWarpDrive".to_string(), false)),
            "moduleBonusAfterburner" | "speedBoost" => {
                Some(("effects.Afterburner".to_string(), false))
            }
            "shieldBoosting" | "fueledShieldBoosting" => {
                Some(("effects.ShieldBoosting".to_string(), false))
            }
            "armorRepair" | "fueledArmorRepair" => {
                Some(("effects.ArmorRepair".to_string(), false))
            }
            _ => None,
        },
    }
}
