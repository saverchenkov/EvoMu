//! Cosmetic (SKIN) table loaders and on-demand celestial queries for `SdeRegistry` (`D-2`),
//! split from `sqlite_loader.rs` to keep each source file under the line cap.

#![forbid(unsafe_code)]

use super::*;

/// Maps a JSON text column decode failure onto `rusqlite::Error::FromSqlConversionFailure`.
pub(crate) fn parse_json_col<T: serde::de::DeserializeOwned>(
    col: usize,
    s: &str,
) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e))
    })
}

type SkinTables = (
    HashMap<TypeId, SkinLicenseInfo>,
    HashMap<u32, SkinMaterialInfo>,
    HashMap<u32, SkinInfo>,
);

/// Loads `sde_skin_licenses`, the optional `sde_skin_materials`, and `sde_skins`.
pub(crate) fn load_skin_tables(conn: &Connection) -> Result<SkinTables, SdeError> {
    let mut skin_licenses = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT license_type_id, skin_id, duration FROM sde_skin_licenses")?;
        let rows = stmt.query_map([], |r| {
            Ok(SkinLicenseInfo {
                license_type_id: r.get::<_, i64>(0)? as TypeId,
                skin_id: r.get::<_, i64>(1)? as u32,
                duration: r.get::<_, i64>(2)? as i32,
            })
        })?;
        for r in rows {
            let sl = r?;
            skin_licenses.insert(sl.license_type_id, sl);
        }
    }

    let mut skin_materials = HashMap::new();
    {
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sde_skin_materials'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if table_exists {
            let mut stmt = conn.prepare(
                "SELECT skin_material_id, display_name_id, material_set_id FROM sde_skin_materials",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SkinMaterialInfo {
                    skin_material_id: r.get::<_, i64>(0)? as u32,
                    display_name_id: r.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    material_set_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                })
            })?;
            for r in rows {
                let sm = r?;
                skin_materials.insert(sm.skin_material_id, sm);
            }
        }
    }

    let mut skins = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT skin_id, internal_name, skin_material_id, types_json, allow_ccp_devs, visible_serenity, visible_tranquility, is_structure_skin, skin_description FROM sde_skins",
        )?;
        let rows = stmt.query_map([], |r| {
            let t_str: String = r.get(3)?;
            Ok(SkinInfo {
                skin_id: r.get::<_, i64>(0)? as u32,
                internal_name: r.get(1)?,
                skin_material_id: r.get::<_, i64>(2)? as u32,
                types: parse_json_col(3, &t_str)?,
                allow_ccp_devs: r.get::<_, i64>(4)? != 0,
                visible_serenity: r.get::<_, i64>(5)? != 0,
                visible_tranquility: r.get::<_, i64>(6)? != 0,
                is_structure_skin: r.get::<_, i64>(7)? != 0,
                skin_description: r.get(8)?,
            })
        })?;
        for r in rows {
            let sk = r?;
            skins.insert(sk.skin_id, sk);
        }
    }

    Ok((skin_licenses, skin_materials, skins))
}

impl SdeRegistry {
    /// Queries celestials (stars, planets, moons, asteroid belts, stargates) in `solar_system_id`
    /// from the on-disk SQLite SDE database (`sde_celestials`, `475,859` records).
    pub fn celestials_in_system(&self, solar_system_id: SolarSystemId) -> Vec<CelestialInfo> {
        let Some(ref p) = self.db_path else {
            return Vec::new();
        };
        let uri = format!("file:{}?immutable=1", p.display());
        let Ok(conn) = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        ) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT celestial_id, system_id, type_id, group_id, name, radius, x, y, z, orbit_id, orbit_index, celestial_index FROM sde_celestials WHERE system_id = ?1 ORDER BY celestial_id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([i64::from(solar_system_id)], |r| {
            Ok(CelestialInfo {
                celestial_id: r.get(0)?,
                solar_system_id: r.get::<_, i64>(1)? as SolarSystemId,
                type_id: r.get::<_, i64>(2)? as TypeId,
                group_id: r.get::<_, i64>(3)? as GroupId,
                name: r.get(4)?,
                radius: r.get(5)?,
                position: DVec3::new(r.get(6)?, r.get(7)?, r.get(8)?),
                orbit_id: r.get(9)?,
                orbit_index: r.get(10)?,
                celestial_index: r.get(11)?,
            })
        }) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Resolves any station (`sde_stations`) or celestial (`sde_celestials`) location ID to its
    /// containing solar system (`res:/staticdata/locationcache.static`).
    pub fn location_solar_system(&self, location_id: i64) -> Option<SolarSystemId> {
        if let Some(sta) = self.stations.get(&location_id) {
            return Some(sta.solar_system_id);
        }
        let p = self.db_path.as_ref()?;
        let uri = format!("file:{}?immutable=1", p.display());
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .ok()?;
        conn.query_row(
            "SELECT system_id FROM sde_celestials WHERE celestial_id = ?1",
            [location_id],
            |r| Ok(r.get::<_, i64>(0)? as SolarSystemId),
        )
        .ok()
    }

    /// Loads the complete `location_id -> solar_system_id` index across all celestials and NPC
    /// stations (`res:/staticdata/locationcache.static`, `481,068` entries).
    pub fn location_solar_system_map(&self) -> HashMap<i64, SolarSystemId> {
        let mut out = HashMap::with_capacity(self.celestial_count + self.stations.len());
        for (&sta_id, sta) in &self.stations {
            out.insert(sta_id, sta.solar_system_id);
        }
        if let Some(ref p) = self.db_path {
            let uri = format!("file:{}?immutable=1", p.display());
            if let Ok(conn) = Connection::open_with_flags(
                &uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            ) {
                if let Ok(mut stmt) =
                    conn.prepare("SELECT celestial_id, system_id FROM sde_celestials")
                {
                    if let Ok(rows) = stmt.query_map([], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as SolarSystemId))
                    }) {
                        for (cid, sid) in rows.flatten() {
                            out.insert(cid, sid);
                        }
                    }
                }
            }
        }
        out
    }

    /// Sorted distinct faction IDs that hold sovereignty over at least one solar system
    /// (`res:/staticdata/factionsowningsolarsystems.static`, `22` factions).
    pub fn factions_owning_solar_systems(&self) -> Vec<u32> {
        let Some(ref p) = self.db_path else {
            return Vec::new();
        };
        let uri = format!("file:{}?immutable=1", p.display());
        let Ok(conn) = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        ) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT faction_id FROM sde_solar_systems WHERE faction_id IS NOT NULL ORDER BY faction_id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| Ok(r.get::<_, i64>(0)? as u32)) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Sorted distinct non-ice asteroid (`category_id == 25 && group_id != 465`) type IDs present
    /// in any room of `dungeon_id` (`res:/staticdata/oretypesbydungeonid.static`).
    pub fn dungeon_ore_types(&self, dungeon_id: u32) -> Option<Vec<TypeId>> {
        let entries = self.dungeon_asteroids.get(&dungeon_id)?;
        let mut ores: Vec<TypeId> = entries
            .iter()
            .filter_map(|e| {
                let t = self.types.get(&e.type_id)?;
                (t.category_id == 25 && t.group_id != 465).then_some(e.type_id)
            })
            .collect();
        ores.sort_unstable();
        ores.dedup();
        (!ores.is_empty()).then_some(ores)
    }

    /// Sorted distinct ice (`group_id == 465`) type IDs present in any room of `dungeon_id`
    /// (`res:/staticdata/icetypesbydungeonid.static`).
    pub fn dungeon_ice_types(&self, dungeon_id: u32) -> Option<Vec<TypeId>> {
        let entries = self.dungeon_asteroids.get(&dungeon_id)?;
        let mut ices: Vec<TypeId> = entries
            .iter()
            .filter_map(|e| {
                let t = self.types.get(&e.type_id)?;
                (t.group_id == 465).then_some(e.type_id)
            })
            .collect();
        ices.sort_unstable();
        ices.dedup();
        (!ices.is_empty()).then_some(ices)
    }

    /// Reverse material index mapping each `material_type_id` to the sorted list of blueprint type
    /// IDs (one entry per activity consuming that material) across `self.blueprints`
    /// (`res:/staticdata/blueprintsbymaterialtypeids.pickle`).
    pub fn blueprints_by_material_type(&self) -> HashMap<TypeId, Vec<TypeId>> {
        let mut out: HashMap<TypeId, Vec<TypeId>> = HashMap::new();
        for (&bp_id, bp) in &self.blueprints {
            for act in bp.activities.values() {
                for mat in &act.materials {
                    out.entry(mat.type_id).or_default().push(bp_id);
                }
            }
        }
        for list in out.values_mut() {
            list.sort_unstable();
        }
        out
    }

    /// Direct required skills (`skill_type_id -> level`) for `type_id` decoded from its dogma
    /// required-skill attribute pairs (`182/277`, `183/278`, `184/279`, `1285/1286`, `1289/1287`,
    /// `1290/1288`; `res:/staticdata/requiredskillsfortypes.fsdbinary`).
    pub fn required_skills_for_type(&self, type_id: TypeId) -> HashMap<TypeId, u8> {
        const PAIRS: [(AttributeId, AttributeId); 6] = [
            (182, 277),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1287),
            (1290, 1288),
        ];
        let Some(inv) = self.types.get(&type_id) else {
            return HashMap::new();
        };
        let mut out = HashMap::new();
        for (sk_attr, lvl_attr) in PAIRS {
            if let (Some(&sk), Some(&lvl)) = (
                inv.default_attributes.get(&sk_attr),
                inv.default_attributes.get(&lvl_attr),
            ) {
                let sk_id = sk as TypeId;
                if sk_id > 0 {
                    out.insert(sk_id, lvl as u8);
                }
            }
        }
        out
    }

    /// Walks `self.market_groups` upward to the top-level market group ID for `market_group_id`.
    pub fn root_market_group_id(&self, mut market_group_id: u32) -> u32 {
        while let Some(parent) = self
            .market_groups
            .get(&market_group_id)
            .and_then(|mg| mg.parent_group_id)
        {
            market_group_id = parent;
        }
        market_group_id
    }

    /// Builds the `skill_type_id -> skill_level -> root_market_group_id -> meta_group_id ->
    /// sorted Vec<TypeId>` index across published types and manufacturing blueprints
    /// (`res:/staticdata/requiredskillsindex.fsdbinary`).
    pub fn required_skills_index(
        &self,
    ) -> HashMap<TypeId, HashMap<u8, HashMap<u32, HashMap<u32, Vec<TypeId>>>>> {
        let mut out: HashMap<TypeId, HashMap<u8, HashMap<u32, HashMap<u32, Vec<TypeId>>>>> =
            HashMap::new();
        for (&tid, inv) in &self.types {
            if !inv.published {
                continue;
            }
            let Some(root_mg) = inv
                .market_group_id
                .map(|mg| self.root_market_group_id(mg))
                .or_else(|| (inv.category_id == 9).then_some(2))
            else {
                continue;
            };
            let meta_group = inv.meta_group_id.unwrap_or(1);
            let mut skills = self.required_skills_for_type(tid);
            if let Some(mfg) = self
                .blueprints
                .get(&tid)
                .and_then(|bp| bp.activities.get("manufacturing"))
            {
                for sk in &mfg.skills {
                    skills.entry(sk.type_id).or_insert(sk.level);
                }
            }
            for (sk_id, lvl) in skills {
                out.entry(sk_id)
                    .or_default()
                    .entry(lvl)
                    .or_default()
                    .entry(root_mg)
                    .or_default()
                    .entry(meta_group)
                    .or_default()
                    .push(tid);
            }
        }
        for lvl_map in out.values_mut() {
            for mg_map in lvl_map.values_mut() {
                for meta_map in mg_map.values_mut() {
                    for tids in meta_map.values_mut() {
                        tids.sort_unstable();
                    }
                }
            }
        }
        out
    }
}
