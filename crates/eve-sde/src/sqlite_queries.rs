//! Explicit `SdeCache` handle and targeted single-domain SQLite queries (`#1583` Track 14).
//!
//! Keeps production code free of process-global mutable `SDE_CACHE` state while ensuring
//! fallback helpers without a `&SdeRegistry` never reload the full 92 MB database.

#![forbid(unsafe_code)]

use super::*;
use std::collections::{HashSet, VecDeque};

type SdeCacheSlot = Arc<Mutex<Option<Arc<SdeRegistry>>>>;

/// Caller-owned, in-memory cache of SQLite-backed [`SdeRegistry`] handles (`#1583` Track 14).
#[derive(Debug, Default)]
pub struct SdeCache {
    slots: Mutex<HashMap<(PathBuf, u64), SdeCacheSlot>>,
}

impl SdeCache {
    /// Creates an empty [`SdeCache`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens an on-disk SQLite SDE database and returns a shared handle deduplicated by
    /// `(canonical_path, byte_len)` within this [`SdeCache`] instance (`D-2`).
    pub fn open_shared(&self, path: impl AsRef<Path>) -> Result<Arc<SdeRegistry>, SdeError> {
        let p = path.as_ref();
        let meta =
            std::fs::metadata(p).map_err(|_| SdeError::MissingDatabase(p.display().to_string()))?;
        if !meta.is_file() {
            return Err(SdeError::MissingDatabase(p.display().to_string()));
        }
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let cache_key = (canon.clone(), meta.len());

        let slot: SdeCacheSlot = {
            let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(map.entry(cache_key).or_default())
        };
        let mut entry = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = entry.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let registry = Arc::new(SdeRegistry::load_uncached(canon)?);
        *entry = Some(Arc::clone(&registry));
        Ok(registry)
    }

    /// Opens an on-disk SQLite SDE database through this [`SdeCache`] and clones it.
    pub fn open(&self, path: impl AsRef<Path>) -> Result<SdeRegistry, SdeError> {
        self.open_shared(path).map(|sde| (*sde).clone())
    }

    /// Opens the canonical runtime SDE database (`SdeRegistry::default_db_path()`) through this
    /// [`SdeCache`], panicking clearly if it is missing (`D-2`).
    pub fn canonical_shared(&self) -> Arc<SdeRegistry> {
        let p = SdeRegistry::default_db_path();
        self.open_shared(&p).unwrap_or_else(|e| {
            panic!(
                "Failed to open runtime SDE SQLite database at '{}': {e}",
                p.display()
            )
        })
    }

    /// Opens and clones the canonical runtime SDE database through this [`SdeCache`].
    pub fn with_canonical_seed(&self) -> SdeRegistry {
        (*self.canonical_shared()).clone()
    }
}

impl SdeRegistry {
    /// Loads only `sde_races` plus the transitive required-skill `sde_types` closure for
    /// `root_type_ids` and racial starter skills (`#1583` Track 14).
    pub fn open_races_and_skill_closure_for_types(
        path: impl AsRef<Path>,
        root_type_ids: &[TypeId],
    ) -> Result<Self, SdeError> {
        let (canon, conn) = Self::open_validated_readonly_conn(path.as_ref())?;
        let races = Self::load_races_table(&conn)?;

        let mut registry = Self {
            db_path: Some(canon),
            races,
            ..Self::default()
        };

        let mut queued: HashSet<TypeId> = HashSet::new();
        let mut pending: VecDeque<TypeId> = VecDeque::new();
        for &tid in root_type_ids {
            if tid > 0 && queued.insert(tid) {
                pending.push_back(tid);
            }
        }
        for race in registry.races.values() {
            for &skill_id in race.starter_skills.keys() {
                if skill_id > 0 && queued.insert(skill_id) {
                    pending.push_back(skill_id);
                }
            }
        }

        while !pending.is_empty() {
            let mut batch: Vec<TypeId> = Vec::with_capacity(pending.len().min(200));
            while batch.len() < 200 {
                let Some(tid) = pending.pop_front() else {
                    break;
                };
                batch.push(tid);
            }
            if batch.is_empty() {
                break;
            }
            let placeholders = (1..=batch.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql_where = format!("type_id IN ({placeholders})");
            let sql_params: Vec<i64> = batch.iter().map(|&id| i64::from(id)).collect();
            let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params
                .iter()
                .map(|v| v as &dyn rusqlite::ToSql)
                .collect();
            let loaded = Self::query_inv_types_where(&conn, Some(&sql_where), &param_refs)?;
            registry.types.extend(loaded.types);
            for tid in batch {
                for (skill_id, req_lvl) in registry.required_skills_for_type(tid) {
                    if req_lvl > 0 && skill_id > 0 && queued.insert(skill_id) {
                        pending.push_back(skill_id);
                    }
                }
            }
        }

        Ok(registry)
    }

    /// Queries `(type_id, group_id, rig_size_attr_1547, cap_size_attr_1785)` rows for all ship
    /// types (`category_id = 6`) without loading unrelated SDE tables (`#1583` Track 14).
    pub fn load_ship_category_type_rows(
        path: impl AsRef<Path>,
    ) -> Result<Vec<(TypeId, GroupId, f64, f64)>, SdeError> {
        let (_, conn) = Self::open_validated_readonly_conn(path.as_ref())?;
        let mut stmt = conn.prepare(
            "SELECT type_id, group_id, attributes_json FROM sde_types WHERE category_id = 6",
        )?;
        let rows = stmt.query_map([], |r| {
            let tid = r.get::<_, i64>(0)? as TypeId;
            let gid = r.get::<_, i64>(1)? as GroupId;
            let attrs_str: String = r.get(2)?;
            let raw_attrs: HashMap<String, f64> =
                crate::sqlite_loader_skins::parse_json_col(2, &attrs_str)?;
            let rig_size = raw_attrs.get("1547").copied().unwrap_or(0.0);
            let is_cap_size = raw_attrs.get("1785").copied().unwrap_or(0.0);
            Ok((tid, gid, rig_size, is_cap_size))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Queries finite positive `base_price` values from `sde_types` without loading full type
    /// attributes or celestial indices (`#1583` Track 14).
    pub fn load_type_base_prices(path: impl AsRef<Path>) -> Result<HashMap<TypeId, f64>, SdeError> {
        let (_, conn) = Self::open_validated_readonly_conn(path.as_ref())?;
        let mut stmt =
            conn.prepare("SELECT type_id, base_price FROM sde_types WHERE base_price > 0.0")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)? as TypeId, r.get::<_, f64>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (tid, price) = row?;
            if price.is_finite() && price > 0.0 {
                out.insert(tid, price);
            }
        }
        Ok(out)
    }

    /// Opens only `sde_resources` and V24 Quasar progression/career/achievement tables
    /// (`#1583` Track 14).
    pub fn open_progression_resources_only(path: impl AsRef<Path>) -> Result<Self, SdeError> {
        let (canon, conn) = Self::open_validated_readonly_conn(path.as_ref())?;
        let resources = Self::load_resources_table(&conn)?;
        let (
            achievements_static,
            achievement_events_static,
            achievement_category_labels,
            career_paths_static,
            career_goal_definitions,
            contribution_methods,
            daily_goal_catalog,
        ) = crate::sqlite_loader_skins::load_v24_progression_tables(&conn)?;

        Ok(Self {
            db_path: Some(canon),
            resources,
            achievements_static,
            achievement_events_static,
            achievement_category_labels,
            career_paths_static,
            career_goal_definitions,
            contribution_methods,
            daily_goal_catalog,
            ..Self::default()
        })
    }

    /// Loads only the metadata needed to evaluate wormhole classification and phenomena for a
    /// single `solar_system_id` (`#1583` Track 14).
    pub fn open_for_wormhole_system(
        path: impl AsRef<Path>,
        solar_system_id: SolarSystemId,
    ) -> Result<Self, SdeError> {
        const METERS_PER_LY: f64 = 9_460_730_472_580_800.0;
        let (canon, conn) = Self::open_validated_readonly_conn(path.as_ref())?;
        let resources = Self::load_resources_table(&conn)?;

        let mut solar_systems = HashMap::new();
        let mut constellations = HashMap::new();
        let mut regions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT system_id, constellation_id, region_id, name, security, x, y, z, radius, planet_ids_json, wormhole_class_id FROM sde_solar_systems WHERE system_id = ?1",
            )?;
            let mut rows = stmt.query_map([i64::from(solar_system_id)], |r| {
                let sys_id = r.get::<_, i64>(0)? as SolarSystemId;
                let x: f64 = r.get(5)?;
                let y: f64 = r.get(6)?;
                let z: f64 = r.get(7)?;
                let pl_str: String = r.get(9)?;
                let planet_ids: Vec<i64> =
                    crate::sqlite_loader_skins::parse_json_col(9, &pl_str)?;
                let wh_class: Option<i32> = r.get::<_, Option<i64>>(10)?.map(|v| v as i32);
                Ok(SolarSystem {
                    solar_system_id: sys_id,
                    constellation_id: r.get::<_, i64>(1)? as u32,
                    region_id: r.get::<_, i64>(2)? as u32,
                    name: r.get(3)?,
                    security_status: r.get(4)?,
                    radius: r.get(8)?,
                    galactic_pos_ly: [x / METERS_PER_LY, y / METERS_PER_LY, z / METERS_PER_LY],
                    planet_ids,
                    stations: Vec::new(),
                    wormhole_class_id: wh_class,
                })
            })?;
            if let Some(row) = rows.next() {
                let sys = row?;
                let const_id = sys.constellation_id;
                let reg_id = sys.region_id;
                solar_systems.insert(sys.solar_system_id, sys);

                let mut cstmt = conn.prepare(
                    "SELECT constellation_id, region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_constellations WHERE constellation_id = ?1",
                )?;
                let crows = cstmt.query_map([i64::from(const_id)], |r| {
                    Ok(ConstellationInfo {
                        constellation_id: r.get::<_, i64>(0)? as u32,
                        region_id: r.get::<_, i64>(1)? as u32,
                        name: r.get(2)?,
                        center: [r.get(3)?, r.get(4)?, r.get(5)?],
                        faction_id: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                        wormhole_class_id: r.get::<_, Option<i64>>(7)?.map(|v| v as i32),
                    })
                })?;
                for cr in crows {
                    let c = cr?;
                    constellations.insert(c.constellation_id, c);
                }

                let mut rstmt = conn.prepare(
                    "SELECT region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_regions WHERE region_id = ?1",
                )?;
                let rrows = rstmt.query_map([i64::from(reg_id)], |r| {
                    Ok(RegionInfo {
                        region_id: r.get::<_, i64>(0)? as u32,
                        name: r.get(1)?,
                        center: [r.get(2)?, r.get(3)?, r.get(4)?],
                        faction_id: r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                        wormhole_class_id: r.get::<_, Option<i64>>(6)?.map(|v| v as i32),
                    })
                })?;
                for rr in rrows {
                    let r = rr?;
                    regions.insert(r.region_id, r);
                }
            }
        }

        let mut celestials_by_system: HashMap<SolarSystemId, Vec<CelestialInfo>> = HashMap::new();
        let mut celestial_system_by_id: HashMap<i64, SolarSystemId> = HashMap::new();
        if crate::sqlite_loader_skins::table_has_column(&conn, "sde_celestials", "system_id") {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT celestial_id, system_id, type_id, group_id, name, radius, x, y, z, orbit_id, orbit_index, celestial_index FROM sde_celestials WHERE system_id = ?1 ORDER BY celestial_id",
            ) {
                if let Ok(rows) = stmt.query_map([i64::from(solar_system_id)], |r| {
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
                }) {
                    for cel in rows.flatten() {
                        celestial_system_by_id.insert(cel.celestial_id, cel.solar_system_id);
                        celestials_by_system
                            .entry(cel.solar_system_id)
                            .or_default()
                            .push(cel);
                    }
                }
            }
        }

        let loaded_types =
            Self::query_inv_types_where(&conn, Some("group_id IN (920, 995)"), &[])?;

        Ok(Self {
            db_path: Some(canon),
            resources,
            types: loaded_types.types,
            constellations,
            regions,
            solar_systems,
            celestial_count: celestial_system_by_id.len(),
            celestials_by_system: Arc::new(celestials_by_system),
            celestial_system_by_id: Arc::new(celestial_system_by_id),
            ..Self::default()
        })
    }
}
