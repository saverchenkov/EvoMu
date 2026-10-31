//! Regional and security-band asteroid belt ore distribution tables (`feat:mining.belt-ore-distribution`).

#![forbid(unsafe_code)]

impl crate::SdeRegistry {
    /// Returns the base asteroid ore types eligible to spawn in belts for `security_status` and `region_id`.
    pub fn belt_ores_for_security_and_region(&self, security_status: f64, region_id: u32) -> Vec<&crate::InvType> {
        if region_id == 10_000_070 {
            let mut pochven_ores: Vec<&crate::InvType> = self
                .types
                .values()
                .filter(|t| {
                    t.category_id == 25
                        && t.published
                        && (4029..=4031).contains(&t.group_id)
                        && self.groups.get(&t.group_id).is_some_and(|g| g.name == t.name)
                })
                .collect();
            pochven_ores.sort_by_key(|t| t.type_id);
            return pochven_ores;
        }
        let faction_id = self
            .constellations
            .values()
            .find(|c| c.region_id == region_id && c.faction_id.is_some())
            .and_then(|c| c.faction_id)
            .unwrap_or(0);
        let type_ids: &[u32] = if security_status >= 0.95 {
            &[1230, 1228]
        } else if security_status >= 0.85 {
            match faction_id {
                500_003 | 500_007 | 500_008 => &[1230, 1228, 1224],
                500_001 => &[1230, 1228],
                500_004 | 500_002 => &[1230, 1228, 18],
                _ => &[1230, 1228, 18, 1224],
            }
        } else if security_status >= 0.75 {
            match faction_id {
                500_003 | 500_007 | 500_008 => &[1230, 1228, 1224],
                500_001 => &[1230, 1228],
                500_004 => &[1230, 1228, 18],
                500_002 => &[1230, 1228, 18, 1224],
                _ => &[1230, 1228, 18, 1224],
            }
        } else if security_status >= 0.65 {
            match faction_id {
                500_003 | 500_007 | 500_008 => &[1230, 1228, 1224, 20],
                500_001 => &[1230, 1228, 18],
                500_004 => &[1230, 1228, 18, 1227],
                500_002 => &[1230, 1228, 18, 1224],
                _ => &[1230, 1228, 18, 1224],
            }
        } else if security_status >= 0.45 {
            match faction_id {
                500_003 | 500_007 | 500_008 => &[1230, 1228, 1224, 20],
                500_001 => &[1230, 1228, 18, 1224],
                500_004 => &[1230, 1228, 18, 1227],
                500_002 => &[1230, 1228, 18, 1224, 20],
                _ => &[1230, 1228, 18, 1224],
            }
        } else if security_status >= 0.05 {
            match faction_id {
                500_003 | 500_007 | 500_008 => &[1230, 1228, 1224, 20, 1226, 1231, 21],
                500_001 => &[1230, 1228, 18, 1224, 20, 21, 1231],
                500_004 => &[1230, 1228, 18, 1227, 1226, 1231, 21],
                500_002 => &[1230, 1228, 18, 1224, 20, 1227, 21, 1231],
                _ => &[1230, 1228, 18, 1224, 20, 1227, 1226, 21, 1231],
            }
        } else if security_status >= -0.25 {
            &[20, 1223, 1225, 1232, 1229, 19]
        } else if security_status >= -0.65 {
            &[20, 1223, 22, 1225, 1232, 1229, 19]
        } else {
            &[20, 1223, 22, 1225, 1232, 1229, 19, 11396]
        };
        type_ids.iter().filter_map(|tid| self.types.get(tid)).collect()
    }

    /// Returns the base asteroid ore types eligible to spawn in belts for `solar_system_id` (`MN-01`, `MN-02`).
    pub fn belt_ores_for_system(&self, solar_system_id: crate::SolarSystemId) -> Vec<&crate::InvType> {
        let Some(sys) = self.solar_systems.get(&solar_system_id) else {
            return Vec::new();
        };
        self.belt_ores_for_security_and_region(sys.security_status, sys.region_id)
    }
}
