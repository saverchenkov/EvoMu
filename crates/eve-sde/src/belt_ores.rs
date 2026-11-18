//! Canonical asteroid belt & system ore distribution (`feat:mining.belt-ore-distribution`).
//!
//! [`SdeRegistry::belt_ores_for_security_and_region`] resolves base SDE ore types (`category_id == 25 && published`
//! with `type.name == group.name`) by matching real SDE `asteroid_types_by_system` bands when loaded,
//! and otherwise using EVE Online's canonical security-band × Empire Quarter / Nullsec region-class
//! (`F`–`K`) / Pochven / Wormhole distribution.

use std::collections::HashSet;

/// Canonical `OreType` constants for Pochven (`10_000_070`) and Wormholes (`>= 11_000_000`),
/// aligned with V24.02 `ore_const.pyc` (`TRIGLAVIAN_ORES`, `J_SPACE_ORES`) and `Asteroids_and_ore.md`.
const POCHVEN_ORES: &[&str] = &["Spodumain", "Talassonite", "Rakovene", "Bezdnacine"];
const J_SPACE_ORES: &[&str] = &[
    "Pyroxeres",
    "Omber",
    "Kernite",
    "Gneiss",
    "Bistot",
    "Arkonor",
];
const CORE_1_0_ORES: &[&str] = &["Veldspar", "Scordite"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum EmpireQuarter {
    Amarr,
    Caldari,
    Gallente,
    Minmatar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NullsecClass {
    F,
    G,
    H,
    I,
    J,
    K,
}

impl crate::SdeRegistry {
    /// Returns the canonical base ore `InvType`s for `(security_status, region_id)`,
    /// sorted ascending by `(volume, type_id)`.
    pub fn belt_ores_for_security_and_region(
        &self,
        security_status: f64,
        region_id: u32,
    ) -> Vec<&crate::InvType> {
        // Pochven (10_000_070) overrides true-sec with Triglavian ore anomalies.
        if region_id == 10_000_070 {
            return self.collect_base_ore_types(POCHVEN_ORES);
        }
        // Yasna Zakh / Zarzakh (10_001_000) contains no asteroid belts.
        if region_id == 10_001_000 {
            return Vec::new();
        }
        // Wormhole J-space regions (11_000_000..12_000_000, or wormhole_class_id 1..=6 | 12..=18)
        // spawn canonical J-space ores. K-space regions carry SDE wormhole_class_id 7..=9.
        let is_wormhole_region = match self.regions.get(&region_id).and_then(|r| r.wormhole_class_id)
        {
            Some(cls) => matches!(cls, 1..=6 | 12..=18),
            None => (11_000_000..12_000_000).contains(&region_id),
        };
        if is_wormhole_region {
            return self.collect_base_ore_types(J_SPACE_ORES);
        }

        // Core 1.0 highsec (`>= 0.95`) spawns Veldspar & Scordite across all Empires.
        if security_status >= 0.95 {
            return self.collect_base_ore_types(CORE_1_0_ORES);
        }

        // Prefer real SDE belt/system asteroid composition when loaded for K-space regions.
        if !self.asteroid_types_by_system.is_empty() && !self.solar_systems.is_empty() {
            if let Some(group_names) = self.sde_band_ore_group_names(security_status, region_id) {
                let refs: Vec<&str> = group_names.iter().map(String::as_str).collect();
                return self.collect_base_ore_types(&refs);
            }
        }

        // Canonical fallback for synthetic SDE fixtures or true-sec slots absent in a region.
        let names = self.canonical_kspace_ores(security_status, region_id);
        self.collect_base_ore_types(names)
    }

    /// Returns the ore types present in `solar_system_id`.
    /// Uses exact SDE `asteroid_types_by_system` entries (deduplicated and sorted by `(volume, type_id)`)
    /// when non-empty, otherwise falling back to [`Self::belt_ores_for_security_and_region`].
    pub fn belt_ores_for_system(&self, solar_system_id: crate::SolarSystemId) -> Vec<&crate::InvType> {
        let Some(sys) = self.solar_systems.get(&solar_system_id) else {
            return Vec::new();
        };
        if let Some(tids) = self.asteroid_types_by_system.get(&solar_system_id) {
            let mut seen = HashSet::with_capacity(tids.len());
            let mut ores: Vec<&crate::InvType> = tids
                .iter()
                .filter(|tid| seen.insert(**tid))
                .filter_map(|tid| self.types.get(tid))
                .filter(|t| t.category_id == 25 && t.published)
                .collect();
            if !ores.is_empty() {
                ores.sort_by(|a, b| {
                    a.volume
                        .partial_cmp(&b.volume)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.type_id.cmp(&b.type_id))
                });
                return ores;
            }
        }
        self.belt_ores_for_security_and_region(sys.security_status, sys.region_id)
    }

    fn collect_base_ore_types(&self, allowed_group_names: &[&str]) -> Vec<&crate::InvType> {
        if allowed_group_names.is_empty() {
            return Vec::new();
        }
        let mut ores: Vec<&crate::InvType> = self
            .types
            .values()
            .filter(|t| t.category_id == 25 && t.published)
            .filter(|t| {
                self.groups.get(&t.group_id).is_some_and(|g| {
                    g.name == t.name && allowed_group_names.contains(&g.name.as_str())
                })
            })
            .collect();
        ores.sort_by(|a, b| {
            a.volume
                .partial_cmp(&b.volume)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.type_id.cmp(&b.type_id))
        });
        ores
    }

    fn sde_band_ore_group_names(
        &self,
        security_status: f64,
        region_id: u32,
    ) -> Option<Vec<String>> {
        let mut names = HashSet::new();

        for sys in self.solar_systems.values() {
            if sys.region_id != region_id {
                continue;
            }
            let sc = self
                .solar_system_security_classes
                .get(&sys.solar_system_id)
                .map(String::as_str)
                .unwrap_or("");
            let in_band = if security_status >= 0.45 {
                // Exclude Class A (`1.0` core) systems when resolving 0.9–0.5 regional quarter ores,
                // and distinguish Caldari 0.75..0.95 (Class B) from 0.45..0.75 (Class C).
                if sys.security_status < 0.45 || sys.security_status >= 0.95 || sc == "A" {
                    false
                } else if security_status >= 0.75 {
                    sys.security_status >= 0.75
                } else {
                    sys.security_status < 0.75
                }
            } else if security_status > 0.0 {
                if sys.security_status <= 0.0 || sys.security_status >= 0.45 {
                    false
                } else if security_status >= 0.25 {
                    sys.security_status >= 0.25
                } else {
                    sys.security_status < 0.25
                }
            } else {
                sys.security_status <= 0.0 && (sys.security_status - security_status).abs() <= 0.05
            };

            if !in_band {
                continue;
            }
            if let Some(tids) = self.asteroid_types_by_system.get(&sys.solar_system_id) {
                for tid in tids {
                    if let Some(inv_ty) = self.types.get(tid) {
                        if inv_ty.category_id == 25 && inv_ty.published {
                            if let Some(grp) = self.groups.get(&inv_ty.group_id) {
                                names.insert(grp.name.clone());
                            }
                        }
                    }
                }
            }
        }

        if names.is_empty() {
            return None;
        }
        Some(names.into_iter().collect())
    }

    fn resolve_empire_quarter(&self, region_id: u32) -> Option<EmpireQuarter> {
        match region_id {
            10_000_001 | 10_000_020 | 10_000_036 | 10_000_038 | 10_000_043 | 10_000_049
            | 10_000_052 | 10_000_054 | 10_000_065 | 10_000_067 => Some(EmpireQuarter::Amarr),
            10_000_002 | 10_000_016 | 10_000_033 | 10_000_069 => Some(EmpireQuarter::Caldari),
            10_000_032 | 10_000_037 | 10_000_044 | 10_000_048 | 10_000_064 | 10_000_068 => {
                Some(EmpireQuarter::Gallente)
            }
            10_000_028 | 10_000_030 | 10_000_042 => Some(EmpireQuarter::Minmatar),
            _ => {
                let faction_id = self
                    .regions
                    .get(&region_id)
                    .and_then(|r| r.faction_id)
                    .or_else(|| {
                        self.constellations
                            .values()
                            .filter(|c| c.region_id == region_id && c.faction_id.is_some())
                            .min_by_key(|c| c.constellation_id)
                            .and_then(|c| c.faction_id)
                    });
                match faction_id {
                    Some(500_003 | 500_007 | 500_008) => Some(EmpireQuarter::Amarr),
                    Some(500_001) => Some(EmpireQuarter::Caldari),
                    Some(500_004) => Some(EmpireQuarter::Gallente),
                    Some(500_002) => Some(EmpireQuarter::Minmatar),
                    _ => None,
                }
            }
        }
    }

    fn resolve_nullsec_class(region_id: u32) -> Option<NullsecClass> {
        match region_id {
            10_000_005 | 10_000_007 | 10_000_009 | 10_000_031 | 10_000_056 | 10_000_061
            | 10_000_062 => Some(NullsecClass::F),
            10_000_022 | 10_000_039 | 10_000_050 | 10_000_059 | 10_000_060 | 10_000_063 => {
                Some(NullsecClass::G)
            }
            10_000_003 | 10_000_010 | 10_000_015 | 10_000_029 | 10_000_035 | 10_000_045
            | 10_000_055 => Some(NullsecClass::H),
            10_000_006 | 10_000_008 | 10_000_011 | 10_000_012 | 10_000_014 | 10_000_025
            | 10_000_047 => Some(NullsecClass::I),
            10_000_023 | 10_000_041 | 10_000_046 | 10_000_051 | 10_000_057 | 10_000_058 => {
                Some(NullsecClass::J)
            }
            10_000_013 | 10_000_018 | 10_000_021 | 10_000_027 | 10_000_034 | 10_000_040
            | 10_000_053 | 10_000_066 => Some(NullsecClass::K),
            _ => None,
        }
    }

    fn canonical_kspace_ores(&self, sec: f64, region_id: u32) -> &'static [&'static str] {
        if sec >= 0.45 {
            return match self.resolve_empire_quarter(region_id) {
                Some(EmpireQuarter::Amarr) => &["Veldspar", "Scordite", "Pyroxeres"],
                Some(EmpireQuarter::Caldari) if sec >= 0.75 => {
                    &["Veldspar", "Scordite", "Pyroxeres"]
                }
                Some(EmpireQuarter::Caldari) => {
                    &["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"]
                }
                Some(EmpireQuarter::Gallente | EmpireQuarter::Minmatar) => {
                    &["Veldspar", "Scordite", "Plagioclase"]
                }
                None => &["Veldspar", "Scordite", "Pyroxeres", "Plagioclase"],
            };
        }

        if sec > 0.0 {
            return match self.resolve_empire_quarter(region_id) {
                Some(EmpireQuarter::Amarr) if sec >= 0.25 => &["Pyroxeres", "Kernite", "Jaspet"],
                Some(EmpireQuarter::Amarr) => &["Pyroxeres", "Kernite", "Jaspet", "Hemorphite"],
                Some(EmpireQuarter::Caldari) if sec >= 0.25 => &["Pyroxeres", "Kernite"],
                Some(EmpireQuarter::Caldari) => &["Pyroxeres", "Kernite", "Hedbergite"],
                Some(EmpireQuarter::Gallente) if sec >= 0.25 => &["Omber", "Jaspet"],
                Some(EmpireQuarter::Gallente) => &["Omber", "Jaspet", "Hemorphite"],
                Some(EmpireQuarter::Minmatar) if sec >= 0.25 => &["Omber", "Kernite"],
                Some(EmpireQuarter::Minmatar) => &["Omber", "Kernite", "Hedbergite"],
                None if sec >= 0.25 => &["Pyroxeres", "Omber", "Kernite", "Jaspet"],
                None => &[
                    "Pyroxeres",
                    "Omber",
                    "Kernite",
                    "Jaspet",
                    "Hedbergite",
                    "Hemorphite",
                ],
            };
        }

        match Self::resolve_nullsec_class(region_id) {
            Some(NullsecClass::F) => {
                if sec <= -0.75 {
                    &["Pyroxeres", "Bistot", "Arkonor", "Mercoxit"]
                } else if sec <= -0.65 {
                    &["Pyroxeres", "Bistot", "Arkonor"]
                } else if sec <= -0.55 {
                    &["Bistot", "Arkonor"]
                } else if sec <= -0.45 {
                    &["Bistot"]
                } else {
                    &[]
                }
            }
            Some(NullsecClass::G) => {
                if sec <= -0.85 && region_id == 10_000_039 {
                    &["Pyroxeres", "Kernite", "Bistot", "Arkonor", "Mercoxit"]
                } else if sec <= -0.75 {
                    &["Pyroxeres", "Kernite", "Bistot", "Mercoxit"]
                } else if sec <= -0.55 {
                    &["Pyroxeres", "Kernite", "Bistot"]
                } else if sec <= -0.35 {
                    &["Pyroxeres", "Kernite"]
                } else {
                    &["Kernite"]
                }
            }
            Some(NullsecClass::H) => {
                if sec <= -0.85 {
                    &["Pyroxeres", "Kernite", "Bistot", "Mercoxit"]
                } else if sec <= -0.75 {
                    &["Pyroxeres", "Kernite", "Mercoxit"]
                } else if sec <= -0.45 {
                    &["Pyroxeres", "Kernite"]
                } else {
                    &["Pyroxeres"]
                }
            }
            Some(NullsecClass::I) => {
                if sec <= -0.85 {
                    &["Kernite", "Arkonor", "Mercoxit"]
                } else if sec <= -0.75 {
                    &["Arkonor", "Mercoxit"]
                } else if sec <= -0.65 {
                    &["Arkonor"]
                } else {
                    &[]
                }
            }
            Some(NullsecClass::J) => {
                if sec <= -0.85 {
                    &["Pyroxeres", "Bistot", "Arkonor", "Mercoxit"]
                } else if sec <= -0.75 {
                    &["Pyroxeres", "Bistot", "Mercoxit"]
                } else if sec <= -0.45 {
                    &["Pyroxeres", "Bistot"]
                } else {
                    &["Pyroxeres"]
                }
            }
            Some(NullsecClass::K) => {
                // Drone regions (K..K7) shift true-sec bands per region; `sde_band_ore_group_names`
                // handles loaded SDE systems, while synthetic/offline lookups use baseline K-bands.
                if sec <= -0.65 {
                    &["Bistot", "Arkonor", "Mercoxit"]
                } else if sec <= -0.45 {
                    &["Bistot", "Arkonor"]
                } else if sec <= -0.30 {
                    &["Bistot"]
                } else {
                    &[]
                }
            }
            None => {
                if sec <= -0.75 {
                    &["Pyroxeres", "Kernite", "Bistot", "Arkonor", "Mercoxit"]
                } else if sec <= -0.45 {
                    &["Pyroxeres", "Kernite", "Bistot", "Arkonor"]
                } else {
                    &["Pyroxeres", "Kernite", "Bistot"]
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "belt_ores_unit_tests.rs"]
mod belt_ores_unit_tests;
