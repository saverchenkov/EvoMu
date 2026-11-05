//! DERIVED: Server-only regional and security-band asteroid belt ore distribution heuristic.
//!
//! V24.01 `.fsdbinary` client data omits server-only per-constellation asteroid belt ore spawn tables.
//! `belt_ores_for_security_and_region` resolves base SDE ore types (`category_id == 25 && published`
//! where `group.name == type.name`) using post-Redistribution empire, low-sec, null-sec, and Pochven rules.

#![forbid(unsafe_code)]

impl crate::SdeRegistry {
    /// Returns the base asteroid ore types eligible to spawn in belts for `security_status` and `region_id`.
    pub fn belt_ores_for_security_and_region(
        &self,
        security_status: f64,
        region_id: u32,
    ) -> Vec<&crate::InvType> {
        let group_ids: &[u32] = if region_id == 10_000_070 {
            &[4029, 4030, 4031]
        } else {
            let faction_id = self
                .constellations
                .values()
                .filter(|c| c.region_id == region_id && c.faction_id.is_some())
                .min_by_key(|c| c.constellation_id)
                .and_then(|c| c.faction_id)
                .unwrap_or(0);

            if security_status >= 0.85 {
                &[462, 460]
            } else if security_status >= 0.65 {
                match faction_id {
                    500_003 => &[462, 460, 459],      // Amarr: Veldspar, Scordite, Pyroxeres
                    500_001 => &[462, 460, 458],      // Caldari: Veldspar, Scordite, Plagioclase
                    500_004 => &[462, 460, 458],      // Gallente: Veldspar, Scordite, Plagioclase
                    500_002 => &[462, 460, 459, 458], // Minmatar: Veldspar, Scordite, Pyroxeres, Plagioclase
                    _ => &[462, 460, 459, 458],
                }
            } else if security_status >= 0.45 {
                &[462, 460, 459, 458]
            } else if security_status >= 0.05 {
                // Post-Redistribution low-sec (0.4 - 0.1): no Veldspar/Scordite
                &[459, 469, 457, 456, 455, 454]
            } else if security_status >= -0.45 {
                // High null-sec (0.0 to -0.4): Gneiss (453), Crokite (467), Dark Ochre (452), Spodumain (461), Mercoxit (468)
                &[453, 467, 452, 461, 468]
            } else {
                // Deep null-sec (-0.5 to -1.0): Gneiss (453), Crokite (467), Dark Ochre (452), Spodumain (461), Bistot (451), Arkonor (450), Mercoxit (468)
                &[453, 467, 452, 461, 451, 450, 468]
            }
        };

        let mut ores: Vec<&crate::InvType> = self
            .types
            .values()
            .filter(|t| {
                t.category_id == 25
                    && t.published
                    && group_ids.contains(&t.group_id)
                    && self.groups.get(&t.group_id).is_some_and(|g| g.name == t.name)
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

    /// Returns the base asteroid ore types eligible to spawn in belts for `solar_system_id` (`MN-01`, `MN-02`).
    pub fn belt_ores_for_system(&self, solar_system_id: crate::SolarSystemId) -> Vec<&crate::InvType> {
        let Some(sys) = self.solar_systems.get(&solar_system_id) else {
            return Vec::new();
        };
        self.belt_ores_for_security_and_region(sys.security_status, sys.region_id)
    }
}

#[cfg(test)]
#[path = "belt_ores_unit_tests.rs"]
mod belt_ores_unit_tests;

