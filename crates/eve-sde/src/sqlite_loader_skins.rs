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

pub(crate) struct EagerUniverseAuxTables {
    pub celestial_count: usize,
    pub celestials_by_system: HashMap<SolarSystemId, Vec<CelestialInfo>>,
    pub celestial_system_by_id: HashMap<i64, SolarSystemId>,
    pub stars: HashMap<i64, StarInfo>,
    pub solar_system_factions: HashMap<SolarSystemId, u32>,
    pub solar_system_security_classes: HashMap<SolarSystemId, String>,
    pub stargate_meta: HashMap<i64, (TypeId, String)>,
    pub school_starting_stations: HashMap<u32, (u32, Vec<i64>)>,
    pub school_career_agents: HashMap<u32, (u32, Vec<i64>)>,
    pub career_goals: HashMap<u32, CareerGoalTemplate>,
}

/// Eagerly loads celestials, stars, solar-system sovereignty/security classes, stargate type/name
/// metadata, starter school station/agent lists, and AIR career goal templates at startup (`#743`)
/// so runtime lookups never reopen `evomu_sde.sqlite`.
pub(crate) fn load_universe_aux_and_career_tables(
    conn: &Connection,
) -> Result<EagerUniverseAuxTables, SdeError> {
    let mut celestials_by_system: HashMap<SolarSystemId, Vec<CelestialInfo>> = HashMap::new();
    let mut celestial_system_by_id: HashMap<i64, SolarSystemId> = HashMap::new();
    let celestial_count: usize;

    if let Ok(mut stmt) = conn.prepare(
        "SELECT celestial_id, system_id, type_id, group_id, name, radius, x, y, z, orbit_id, orbit_index, celestial_index FROM sde_celestials ORDER BY celestial_id",
    ) {
        let mut loaded = 0usize;
        if let Ok(rows) = stmt.query_map([], |r| {
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
                loaded += 1;
            }
        }
        celestial_count = loaded;
    } else {
        celestial_count = conn
            .query_row("SELECT count(*) FROM sde_celestials", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize;
    }

    let mut stars: HashMap<i64, StarInfo> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT star_id, system_id, type_id, radius, age, luminosity, temperature, spectral_class FROM sde_stars",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(StarInfo {
                star_id: r.get(0)?,
                solar_system_id: r.get::<_, i64>(1)? as SolarSystemId,
                type_id: r.get::<_, i64>(2)? as TypeId,
                radius: r.get(3)?,
                age: r.get(4)?,
                luminosity: r.get(5)?,
                temperature: r.get(6)?,
                spectral_class: r.get(7)?,
            })
        }) {
            for st in rows.flatten() {
                stars.insert(st.star_id, st);
            }
        }
    }

    let mut solar_system_factions: HashMap<SolarSystemId, u32> = HashMap::new();
    let mut solar_system_security_classes: HashMap<SolarSystemId, String> = HashMap::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT system_id, faction_id, security_class FROM sde_solar_systems")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as SolarSystemId,
                r.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                r.get::<_, Option<String>>(2)?,
            ))
        }) {
            for (sys_id, fac_opt, sec_cls_opt) in rows.flatten() {
                if let Some(fac) = fac_opt {
                    solar_system_factions.insert(sys_id, fac);
                }
                if let Some(cls) = sec_cls_opt.filter(|s| !s.is_empty()) {
                    solar_system_security_classes.insert(sys_id, cls);
                }
            }
        }
    } else if let Ok(mut stmt) =
        conn.prepare("SELECT system_id, faction_id FROM sde_solar_systems WHERE faction_id IS NOT NULL")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as SolarSystemId,
                r.get::<_, i64>(1)? as u32,
            ))
        }) {
            for (sys_id, fac) in rows.flatten() {
                solar_system_factions.insert(sys_id, fac);
            }
        }
    }

    let mut stargate_meta: HashMap<i64, (TypeId, String)> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT stargate_id, type_id, name FROM sde_stargates") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as TypeId,
                r.get::<_, String>(2)?,
            ))
        }) {
            for (gid, tid, name) in rows.flatten() {
                stargate_meta.insert(gid, (tid, name));
            }
        }
    }

    let mut school_starting_stations: HashMap<u32, (u32, Vec<i64>)> = HashMap::new();
    let mut school_career_agents: HashMap<u32, (u32, Vec<i64>)> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT school_id, race_id, starting_stations_json, career_agents_json FROM sde_schools",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)? as u32,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        }) {
            for (sid, race_id, sts_opt, ags_opt) in rows.flatten() {
                if let Some(sts) = sts_opt
                    .and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
                    .filter(|v| !v.is_empty())
                {
                    school_starting_stations.insert(sid, (race_id, sts));
                }
                if let Some(ags) = ags_opt
                    .and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
                    .filter(|v| !v.is_empty())
                {
                    school_career_agents.insert(sid, (race_id, ags));
                }
            }
        }
    }

    let career_goals = load_career_goals_table(conn)?;

    Ok(EagerUniverseAuxTables {
        celestial_count,
        celestials_by_system,
        celestial_system_by_id,
        stars,
        solar_system_factions,
        solar_system_security_classes,
        stargate_meta,
        school_starting_stations,
        school_career_agents,
        career_goals,
    })
}

fn load_career_goals_table(conn: &Connection) -> Result<HashMap<u32, CareerGoalTemplate>, SdeError> {
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sde_career_goals'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if table_exists {
        let mut out = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT goal_id, career_path, name, target_progress, sp_reward FROM sde_career_goals",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CareerGoalTemplate {
                goal_id: r.get::<_, i64>(0)? as u32,
                career_path: r.get(1)?,
                name: r.get(2)?,
                target_progress: r.get::<_, i64>(3)?.max(0) as u64,
                sp_reward: r.get::<_, i64>(4)?.max(0) as u64,
            })
        })?;
        for row in rows {
            let goal = row?;
            out.insert(goal.goal_id, goal);
        }
        return Ok(out);
    }

    #[derive(Deserialize)]
    struct RawAirCareerGoal {
        career_name: String,
        name: String,
        target: u64,
        #[serde(default)]
        sp_reward: f64,
        uuid: [u8; 16],
    }

    let raw_goals: Vec<RawAirCareerGoal> =
        serde_json::from_str(include_str!("../../../data/air_career_goals.json"))?;
    let mut out = HashMap::with_capacity(raw_goals.len());
    for g in raw_goals {
        let goal_id = u32::from(g.uuid[2]) * 10_000 + u32::from(g.uuid[15]);
        out.insert(
            goal_id,
            CareerGoalTemplate {
                goal_id,
                career_path: g.career_name,
                name: g.name,
                target_progress: g.target,
                sp_reward: g.sp_reward.max(0.0).round() as u64,
            },
        );
    }
    Ok(out)
}

impl SdeRegistry {
    /// Returns celestials (stars, planets, moons, asteroid belts) in `solar_system_id` from the
    /// eagerly-loaded in-memory index (`sde_celestials`, `475,859` records).
    pub fn celestials_in_system(&self, solar_system_id: SolarSystemId) -> Vec<CelestialInfo> {
        self.celestials_in_system_slice(solar_system_id).to_vec()
    }

    /// Returns all star `(star_id, type_id)` pairs sorted by ID from memory (`sde_stars`, `8,089`
    /// records, falling back to `group_id == 6` celestials when `stars` is empty in synthetic fixtures).
    pub fn all_stars(&self) -> Vec<(i64, TypeId)> {
        let mut out: Vec<(i64, TypeId)> = if !self.stars.is_empty() {
            self.stars
                .values()
                .map(|s| (s.star_id, s.type_id))
                .collect()
        } else {
            self.celestials_by_system
                .values()
                .flat_map(|list| list.iter())
                .filter(|c| c.group_id == 6)
                .map(|c| (c.celestial_id, c.type_id))
                .collect()
        };
        out.sort_unstable_by_key(|&(id, _)| id);
        out
    }

    /// Resolves any station (`sde_stations`) or celestial (`sde_celestials`) location ID to its
    /// containing solar system in $O(1)$ (`res:/staticdata/locationcache.static`).
    pub fn location_solar_system(&self, location_id: i64) -> Option<SolarSystemId> {
        if let Some(sta) = self.stations.get(&location_id) {
            return Some(sta.solar_system_id);
        }
        self.celestial_system_by_id.get(&location_id).copied()
    }

    /// Builds the `location_id -> solar_system_id` index across all celestials and NPC stations
    /// from memory (`res:/staticdata/locationcache.static`, `481,069` entries).
    pub fn location_solar_system_map(&self) -> HashMap<i64, SolarSystemId> {
        let mut out =
            HashMap::with_capacity(self.celestial_system_by_id.len() + self.stations.len());
        for (&sta_id, sta) in &self.stations {
            out.insert(sta_id, sta.solar_system_id);
        }
        for (&cid, &sid) in self.celestial_system_by_id.iter() {
            out.insert(cid, sid);
        }
        out
    }

    /// Sorted distinct faction IDs that hold sovereignty over at least one solar system from memory
    /// (`res:/staticdata/factionsowningsolarsystems.static`, `22` factions).
    pub fn factions_owning_solar_systems(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.solar_system_factions.values().copied().collect();
        out.sort_unstable();
        out.dedup();
        out
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
            if let Some(&sk) = inv.default_attributes.get(&sk_attr) {
                let sk_id = sk as TypeId;
                if sk_id > 0 {
                    let lvl = inv
                        .default_attributes
                        .get(&lvl_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .round()
                        .max(0.0) as u8;
                    out.insert(sk_id, lvl);
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

    /// Joins `fighter_abilities_by_type[fighter_type_id]` with `self.fighter_abilities` so server
    /// fighter controllers can inspect both per-slot cooldowns/charges and ability metadata
    /// (`target_mode`, `disallow_in_high_sec`, `disallow_in_low_sec`, `turret_graphic_id`,
    /// `tooltip_text_id`; `res:/staticdata/fighterabilities.static` &
    /// `res:/staticdata/fighterabilitiesbytype.static`).
    pub fn fighter_ability_slots_with_info(
        &self,
        fighter_type_id: TypeId,
    ) -> Vec<(FighterAbilitySlot, &FighterAbilityInfo)> {
        let Some(slots) = self.fighter_abilities_by_type.get(&fighter_type_id) else {
            return Vec::new();
        };
        slots
            .iter()
            .filter_map(|slot| {
                let info = self.fighter_abilities.get(&slot.ability_id)?;
                Some((slot.clone(), info))
            })
            .collect()
    }

    /// Resolves the on-disk V24.01 client `SharedCache/ResFiles` path for a `res:/...` static
    /// data resource indexed in `self.resources` (overridable via `EVOMU_RESFILES_DIR` or
    /// `EVOMU_SHARED_CACHE`, with cross-platform macOS + Windows `SharedCache` auto-discovery).
    pub fn cdn_resource_path(&self, res_path: &str) -> Result<PathBuf, SdeError> {
        let info = self.resources.get(res_path).ok_or_else(|| {
            SdeError::InvalidSchema(format!("SDE resource not indexed: {res_path}"))
        })?;
        let base = resolve_resfiles_base_dir();
        let path = base.join(&info.cdn_relative_path);
        if !path.is_file() {
            return Err(SdeError::MissingDatabase(path.display().to_string()));
        }
        Ok(path)
    }
}

fn resolve_resfiles_base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("EVOMU_RESFILES_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(sc) = std::env::var("EVOMU_SHARED_CACHE") {
        let trimmed = sc.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            let res = p.join("ResFiles");
            return if res.is_dir() { res } else { p };
        }
    }
    static DISCOVERED_RESFILES_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DISCOVERED_RESFILES_DIR
        .get_or_init(discover_platform_resfiles_dir)
        .clone()
}

fn discover_platform_resfiles_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\CCP\EVEONLINE",
                "/v",
                "CACHEFOLDER",
            ])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    if let Some((_, val)) = line.split_once("REG_SZ") {
                        let folder = val.trim();
                        if !folder.is_empty() {
                            candidates.push(PathBuf::from(folder).join("ResFiles"));
                        }
                    }
                }
            }
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            candidates.push(
                PathBuf::from(home.trim())
                    .join("Library/Application Support/EVE Online/SharedCache/ResFiles"),
            );
        }
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        if !local_app_data.trim().is_empty() {
            candidates.push(
                PathBuf::from(local_app_data.trim()).join(r"CCP\EVE\SharedCache\ResFiles"),
            );
        }
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        if !user_profile.trim().is_empty() {
            candidates.push(
                PathBuf::from(user_profile.trim())
                    .join(r"AppData\Local\CCP\EVE\SharedCache\ResFiles"),
            );
        }
    }
    if let Ok(program_data) = std::env::var("PROGRAMDATA") {
        if !program_data.trim().is_empty() {
            candidates
                .push(PathBuf::from(program_data.trim()).join(r"CCP\EVE\SharedCache\ResFiles"));
        }
    }
    for fixed in [
        r"C:\EVE\SharedCache\ResFiles",
        r"C:\EVE\ResFiles",
        r"C:\Games\EVE\ResFiles",
        r"C:\Games\EVE\SharedCache\ResFiles",
        r"D:\EVE\SharedCache\ResFiles",
        r"D:\Games\EVE\ResFiles",
    ] {
        candidates.push(PathBuf::from(fixed));
    }

    for cand in &candidates {
        if cand.is_dir() {
            return cand.clone();
        }
    }
    candidates.into_iter().next().unwrap_or_else(|| {
        PathBuf::from("Library/Application Support/EVE Online/SharedCache/ResFiles")
    })
}

#[cfg(test)]
#[path = "sqlite_loader_skins_unit_tests.rs"]
mod sqlite_loader_skins_unit_tests;


