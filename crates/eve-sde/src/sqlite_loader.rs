//! SQLite SDE loader implementation for `SdeRegistry` (`D-2`).

#![forbid(unsafe_code)]

use super::*;
use crate::sqlite_loader_skins::parse_json_col;

impl SdeRegistry {
    /// Opens an on-disk SQLite SDE database produced by `tools/sde-import` (`D-2`).
    /// Fails clearly with [`SdeError::MissingDatabase`] if `path` does not exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SdeError> {
        let p = path.as_ref();
        let meta =
            std::fs::metadata(p).map_err(|_| SdeError::MissingDatabase(p.display().to_string()))?;
        if !meta.is_file() {
            return Err(SdeError::MissingDatabase(p.display().to_string()));
        }
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let cache_key = (canon.clone(), meta.len());

        let cache = SDE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let slot: SdeCacheSlot = {
            let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(map.entry(cache_key).or_default())
        };
        let mut entry = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = entry.as_ref() {
            return Ok(existing.clone());
        }
        let registry = Self::load_uncached(canon)?;
        *entry = Some(registry.clone());
        Ok(registry)
    }

    /// Loads the SDE from `canon` without consulting the process-wide cache (`D-2`).
    fn load_uncached(canon: PathBuf) -> Result<Self, SdeError> {
        // Strip Windows extended path prefix `\\?\` for SQLite URI compatibility (SUP-001)
        let path_str = canon.display().to_string();
        let clean_path = if cfg!(windows) && path_str.starts_with(r"\\?\") {
            &path_str[4..]
        } else {
            &path_str
        };
        let uri = format!("file:{}?immutable=1", clean_path);
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        let schema_ver: String = conn
            .query_row(
                "SELECT value FROM sde_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                SdeError::InvalidSchema(format!("missing sde_meta.schema_version: {e}"))
            })?;
        if schema_ver != "1" {
            return Err(SdeError::InvalidSchema(format!(
                "unsupported SDE schema_version '{schema_ver}'"
            )));
        }

        let mut resources = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT res_path, cdn_relative_path, md5, byte_size, format, record_count, spot FROM sde_resources",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SdeResourceInfo {
                    res_path: r.get(0)?,
                    cdn_relative_path: r.get(1)?,
                    md5: r.get(2)?,
                    byte_size: r.get::<_, i64>(3)? as u64,
                    format: r.get(4)?,
                    record_count: r.get::<_, i64>(5)? as usize,
                    spot: r.get(6)?,
                })
            })?;
            for r in rows {
                let info = r?;
                resources.insert(info.res_path.clone(), info);
            }
        }

        let mut categories = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT category_id, name, published FROM sde_categories")?;
            let rows = stmt.query_map([], |r| {
                Ok(CategoryInfo {
                    category_id: r.get::<_, i64>(0)? as CategoryId,
                    name: r.get(1)?,
                    published: r.get::<_, i64>(2)? != 0,
                })
            })?;
            for r in rows {
                let info = r?;
                categories.insert(info.category_id, info);
            }
        }

        let mut groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT group_id, category_id, name, anchorable, anchored, fittable_non_singleton, use_base_price, published FROM sde_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(GroupInfo {
                    group_id: r.get::<_, i64>(0)? as GroupId,
                    category_id: r.get::<_, i64>(1)? as CategoryId,
                    name: r.get(2)?,
                    anchorable: r.get::<_, i64>(3)? != 0,
                    anchored: r.get::<_, i64>(4)? != 0,
                    fittable_non_singleton: r.get::<_, i64>(5)? != 0,
                    use_base_price: r.get::<_, i64>(6)? != 0,
                    published: r.get::<_, i64>(7)? != 0,
                })
            })?;
            for r in rows {
                let info = r?;
                groups.insert(info.group_id, info);
            }
        }

        let mut types = HashMap::new();
        let mut variations_by_parent: HashMap<TypeId, Vec<TypeId>> = HashMap::new();
        let mut wrecks = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_id, group_id, category_id, name, mass, volume, capacity, portion_size, base_price, radius, market_group_id, race_id, faction_id, variation_parent_type_id, wreck_type_id, published, attributes_json, effects_json, description_id, icon_id, graphic_id, sound_id, meta_group_id, meta_level, tech_level, default_effect_id, description FROM sde_types",
            )?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let wreck_tid: Option<i64> = r.get(14)?;
                let attrs_str: String = r.get(16)?;
                let effs_str: String = r.get(17)?;
                let raw_attrs: HashMap<String, f64> = parse_json_col(16, &attrs_str)?;
                let mut default_attributes = HashMap::with_capacity(raw_attrs.len());
                for (k, v) in raw_attrs {
                    if let Ok(aid) = k.parse::<u16>() {
                        default_attributes.insert(aid, v);
                    }
                }
                let raw_effs: Vec<u32> = parse_json_col(17, &effs_str)?;
                let effect_ids: Vec<EffectId> = raw_effs
                    .into_iter()
                    .filter_map(|eid| u16::try_from(eid).ok())
                    .collect();
                let desc_id: Option<u32> = r.get::<_, Option<i64>>(18)?.map(|v| v as u32);
                let icon_id: Option<u32> = r.get::<_, Option<i64>>(19)?.map(|v| v as u32);
                let graphic_id: Option<u32> = r.get::<_, Option<i64>>(20)?.map(|v| v as u32);
                let sound_id: Option<u32> = r.get::<_, Option<i64>>(21)?.map(|v| v as u32);
                let meta_group_id: Option<u32> = r.get::<_, Option<i64>>(22)?.map(|v| v as u32);
                let meta_level: Option<i64> = r.get(23)?;
                let tech_level: Option<u32> = r.get::<_, Option<i64>>(24)?.map(|v| v as u32);
                let default_effect_id: Option<EffectId> = r
                    .get::<_, Option<i64>>(25)?
                    .and_then(|v| u16::try_from(v).ok());
                let description: String = r.get::<_, Option<String>>(26)?.unwrap_or_default();
                Ok((
                    InvType {
                        type_id: tid,
                        group_id: r.get::<_, i64>(1)? as GroupId,
                        category_id: r.get::<_, i64>(2)? as CategoryId,
                        name: r.get(3)?,
                        mass: r.get(4)?,
                        volume: r.get(5)?,
                        capacity: r.get(6)?,
                        portion_size: (r.get::<_, i64>(7)?.max(1)) as u64,
                        base_price: r.get(8)?,
                        radius: r.get(9)?,
                        market_group_id: r.get::<_, Option<i64>>(10)?.map(|v| v as u32),
                        race_id: r.get::<_, Option<i64>>(11)?.map(|v| v as u32),
                        faction_id: r.get::<_, Option<i64>>(12)?.map(|v| v as u32),
                        variation_parent_type_id: r.get::<_, Option<i64>>(13)?.map(|v| v as TypeId),
                        published: r.get::<_, i64>(15)? != 0,
                        description_id: desc_id,
                        icon_id,
                        graphic_id,
                        sound_id,
                        meta_group_id,
                        meta_level,
                        tech_level,
                        default_effect_id,
                        description,
                        effect_ids,
                        default_attributes,
                    },
                    wreck_tid.map(|w| w as TypeId),
                ))
            })?;
            for r in rows {
                let (inv_type, wreck_tid) = r?;
                if let Some(w) = wreck_tid {
                    wrecks.insert(inv_type.type_id, w);
                }
                if let Some(parent_tid) = inv_type.variation_parent_type_id {
                    variations_by_parent
                        .entry(parent_tid)
                        .or_default()
                        .push(inv_type.type_id);
                }
                types.insert(inv_type.type_id, inv_type);
            }
            for list in variations_by_parent.values_mut() {
                list.sort_unstable();
            }
        }

        let mut attributes = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT attribute_id, name, default_value, high_is_good, stackable, published FROM sde_dogma_attributes",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DogmaAttributeType {
                    attribute_id: r.get::<_, i64>(0)? as AttributeId,
                    name: r.get(1)?,
                    default_value: r.get(2)?,
                    high_is_good: r.get::<_, i64>(3)? != 0,
                    stackable: r.get::<_, i64>(4)? != 0,
                    published: r.get::<_, i64>(5)? != 0,
                })
            })?;
            for r in rows {
                let attr = r?;
                attributes.insert(attr.attribute_id, attr);
            }
        }

        let mut effects = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT effect_id, name, effect_category, is_offensive, is_assistance, is_warp_safe, disallow_auto_repeat, electronic_chance, propulsion_chance, range_chance, guid, published, modifiers_json FROM sde_dogma_effects",
            )?;
            let rows = stmt.query_map([], |r| {
                let eid = r.get::<_, i64>(0)? as EffectId;
                let is_warp_safe = r.get::<_, i64>(5)? != 0;
                let disallow_auto_repeat = r.get::<_, i64>(6)? != 0;
                let electronic_chance = r.get::<_, i64>(7)? != 0;
                let propulsion_chance = r.get::<_, i64>(8)? != 0;
                let range_chance = r.get::<_, i64>(9)? != 0;
                let guid: Option<String> = r.get(10)?;
                let published = r.get::<_, i64>(11)? != 0;
                let mods_str: String = r.get(12)?;
                let raw_mods: Vec<RawModifier> = parse_json_col(12, &mods_str)?;
                let modifiers = raw_mods
                    .into_iter()
                    .filter_map(|m| {
                        let func = m.func?;
                        let domain = m.domain.unwrap_or_else(|| "shipID".to_string());
                        let effect_id = m.effect_id.and_then(|v| u16::try_from(v).ok());
                        if func == "EffectStopper" {
                            Some(EffectModifierSpec {
                                domain,
                                func,
                                modified_attribute_id: m
                                    .modified_attribute_id
                                    .and_then(|v| u16::try_from(v).ok())
                                    .unwrap_or(0),
                                modifying_attribute_id: m
                                    .modifying_attribute_id
                                    .and_then(|v| u16::try_from(v).ok())
                                    .unwrap_or(0),
                                operator: m.operator.unwrap_or(0),
                                group_id: m.group_id,
                                skill_type_id: m.skill_type_id,
                                effect_id,
                            })
                        } else {
                            Some(EffectModifierSpec {
                                domain,
                                func,
                                modified_attribute_id: u16::try_from(m.modified_attribute_id?)
                                    .ok()?,
                                modifying_attribute_id: u16::try_from(m.modifying_attribute_id?)
                                    .ok()?,
                                operator: m.operator.unwrap_or(0),
                                group_id: m.group_id,
                                skill_type_id: m.skill_type_id,
                                effect_id,
                            })
                        }
                    })
                    .collect();
                Ok(DogmaEffectType {
                    effect_id: eid,
                    name: r.get(1)?,
                    effect_category: r.get::<_, i64>(2)? as u16,
                    is_offensive: r.get::<_, i64>(3)? != 0,
                    is_assistance: r.get::<_, i64>(4)? != 0,
                    is_warp_safe,
                    disallow_auto_repeat,
                    electronic_chance,
                    propulsion_chance,
                    range_chance,
                    guid,
                    published,
                    modifiers,
                })
            })?;
            for r in rows {
                let eff = r?;
                effects.insert(eff.effect_id, eff);
            }
        }

        let mut type_materials = HashMap::new();
        let mut randomized_type_materials = HashMap::new();
        {
            let has_rand = conn
                .prepare("SELECT randomized_materials_json FROM sde_type_materials LIMIT 0")
                .is_ok();
            let sql = if has_rand {
                "SELECT type_id, materials_json, randomized_materials_json FROM sde_type_materials"
            } else {
                "SELECT type_id, materials_json, '[]' FROM sde_type_materials"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let m_str: String = r.get(1)?;
                let rm_str: String = r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "[]".into());
                let entries: Vec<TypeMaterialEntry> = parse_json_col(1, &m_str)?;
                let rand_entries: Vec<RandomizedMaterialEntry> = parse_json_col(2, &rm_str)?;
                Ok((tid, entries, rand_entries))
            })?;
            for r in rows {
                let (tid, entries, rand_entries) = r?;
                type_materials.insert(tid, entries);
                if !rand_entries.is_empty() {
                    randomized_type_materials.insert(tid, rand_entries);
                }
            }
        }

        let mut blueprints = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT blueprint_type_id, product_type_id, product_quantity, manufacturing_time_sec, max_production_limit, materials_json, activities_json FROM sde_blueprints",
            )?;
            let rows = stmt.query_map([], |r| {
                let bpid = r.get::<_, i64>(0)? as TypeId;
                let prod_tid = r.get::<_, i64>(1)? as TypeId;
                let prod_qty = r.get::<_, i64>(2)? as u32;
                let mfg_time = r.get::<_, i64>(3)? as u64;
                let max_prod_limit = r.get::<_, i64>(4)?.max(0) as u32;
                let mats_str: String = r.get(5)?;
                let acts_str: String = r.get(6)?;
                let raw_mats: Vec<TypeMaterialEntry> = parse_json_col(5, &mats_str)?;
                let materials = raw_mats
                    .into_iter()
                    .map(|m| BlueprintMaterial {
                        type_id: m.material_type_id,
                        quantity: m.quantity,
                    })
                    .collect();
                let activities: HashMap<String, BlueprintActivity> = parse_json_col(6, &acts_str)?;
                let me_time = activities
                    .get("research_material")
                    .map(|a| a.time)
                    .unwrap_or(0);
                let te_time = activities.get("research_time").map(|a| a.time).unwrap_or(0);
                let copy_time = activities.get("copying").map(|a| a.time).unwrap_or(0);
                let inv_time = activities.get("invention").map(|a| a.time).unwrap_or(0);
                let inv_prob = activities
                    .get("invention")
                    .and_then(|a| a.products.first())
                    .and_then(|p| p.probability)
                    .unwrap_or(0.34);
                Ok(BlueprintInfo {
                    blueprint_type_id: bpid,
                    product_type_id: prod_tid,
                    product_quantity: prod_qty,
                    manufacturing_time_sec: mfg_time,
                    max_production_limit: max_prod_limit,
                    research_me_time_sec: me_time,
                    research_te_time_sec: te_time,
                    copy_time_sec: copy_time,
                    invention_time_sec: inv_time,
                    invention_base_probability: inv_prob,
                    materials,
                    activities,
                })
            })?;
            for r in rows {
                let bp = r?;
                blueprints.insert(bp.blueprint_type_id, bp);
            }
        }

        let mut regions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_regions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(RegionInfo {
                    region_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    center: [r.get(2)?, r.get(3)?, r.get(4)?],
                    faction_id: r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    wormhole_class_id: r.get::<_, Option<i64>>(6)?.map(|v| v as i32),
                })
            })?;
            for r in rows {
                let reg = r?;
                regions.insert(reg.region_id, reg);
            }
        }

        let mut constellations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT constellation_id, region_id, name, x, y, z, faction_id, wormhole_class_id FROM sde_constellations",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(ConstellationInfo {
                    constellation_id: r.get::<_, i64>(0)? as u32,
                    region_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                    center: [r.get(3)?, r.get(4)?, r.get(5)?],
                    faction_id: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    wormhole_class_id: r.get::<_, Option<i64>>(7)?.map(|v| v as i32),
                })
            })?;
            for r in rows {
                let c = r?;
                constellations.insert(c.constellation_id, c);
            }
        }

        let mut op_has_clone_bay = HashMap::new();
        let mut station_operations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT operation_id, name, activity_id, services_json FROM sde_station_operations",
            )?;
            let rows = stmt.query_map([], |r| {
                let oid = r.get::<_, i64>(0)?;
                let name: String = r.get(1)?;
                let activity_id = r.get::<_, Option<i64>>(2)?.map(|v| v as u32);
                let svcs_str: String = r.get(3)?;
                let svcs: Vec<i64> = parse_json_col(3, &svcs_str)?;
                let has_cb = svcs.contains(&10) || svcs.contains(&24);
                let info = StationOperationInfo {
                    operation_id: oid as u32,
                    name,
                    activity_id,
                    services: svcs.into_iter().map(|s| s as u32).collect(),
                };
                Ok((oid, has_cb, info))
            })?;
            for r in rows {
                let (oid, has_cb, info) = r?;
                op_has_clone_bay.insert(oid, has_cb);
                station_operations.insert(info.operation_id, info);
            }
        }

        let mut station_services = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT service_id, name FROM sde_station_services")?;
            let rows = stmt.query_map([], |r| {
                Ok(StationServiceInfo {
                    service_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                })
            })?;
            for r in rows {
                let svc = r?;
                station_services.insert(svc.service_id, svc);
            }
        }

        let mut factions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT faction_id, name, corporation_id, militia_corporation_id, solar_system_id, size_factor, description FROM sde_factions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FactionInfo {
                    faction_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    corporation_id: r.get(2)?,
                    militia_corporation_id: r.get(3)?,
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    size_factor: r.get::<_, Option<f64>>(5)?.unwrap_or(1.0),
                    description: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                })
            })?;
            for r in rows {
                let f = r?;
                factions.insert(f.faction_id, f);
            }
        }

        let mut npc_corporations = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT corporation_id, name, ticker_name, faction_id, solar_system_id, station_id, tax_rate FROM sde_npc_corporations",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(NpcCorporationInfo {
                    corporation_id: r.get(0)?,
                    name: r.get(1)?,
                    ticker_name: r.get(2)?,
                    faction_id: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    station_id: r.get(5)?,
                    tax_rate: r.get(6)?,
                })
            })?;
            for r in rows {
                let c = r?;
                npc_corporations.insert(c.corporation_id, c);
            }
        }

        let mut stations: HashMap<i64, StationInfo> = HashMap::new();
        let mut stations_by_system: HashMap<SolarSystemId, Vec<StationInfo>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT station_id, system_id, constellation_id, region_id, owner_id, type_id, operation_id, name, x, y, z, reprocessing_efficiency, reprocessing_stations_take, is_conquerable, orbit_id FROM sde_stations ORDER BY station_id",
            )?;
            let rows = stmt.query_map([], |r| {
                let sta_id: i64 = r.get(0)?;
                let sys_id = r.get::<_, i64>(1)? as SolarSystemId;
                let const_id = r.get::<_, i64>(2)? as u32;
                let reg_id = r.get::<_, i64>(3)? as u32;
                let owner_id: i64 = r.get(4)?;
                let tid = r.get::<_, i64>(5)? as TypeId;
                let op_id: i64 = r.get(6)?;
                let name: String = r.get(7)?;
                let x: f64 = r.get(8)?;
                let y: f64 = r.get(9)?;
                let z: f64 = r.get(10)?;
                let reproc_eff: f64 = r.get(11)?;
                let reproc_take: f64 = r.get(12)?;
                let is_conq = r.get::<_, i64>(13)? != 0;
                let orbit_id: Option<i64> = r.get(14)?;
                let faction_id = npc_corporations
                    .get(&owner_id)
                    .and_then(|c| c.faction_id)
                    .map(|f| f as i64);
                let has_clone_bay = op_has_clone_bay.get(&op_id).copied().unwrap_or(true);
                Ok((
                    sys_id,
                    StationInfo {
                        station_id: sta_id,
                        solar_system_id: sys_id,
                        constellation_id: const_id,
                        region_id: reg_id,
                        station_type_id: tid,
                        name,
                        position: DVec3::new(x, y, z),
                        owner_corporation_id: owner_id,
                        owner_faction_id: faction_id,
                        operation_id: op_id as u32,
                        reprocessing_efficiency: reproc_eff,
                        reprocessing_stations_take: reproc_take,
                        is_conquerable: is_conq,
                        orbit_id,
                        has_clone_bay,
                    },
                ))
            })?;
            for r in rows {
                let (sys_id, sta) = r?;
                stations.insert(sta.station_id, sta.clone());
                stations_by_system.entry(sys_id).or_default().push(sta);
            }
        }

        const METERS_PER_LY: f64 = 9_460_730_472_580_800.0;
        let mut solar_systems = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT system_id, constellation_id, region_id, name, security, x, y, z, radius, planet_ids_json, wormhole_class_id FROM sde_solar_systems",
            )?;
            let rows = stmt.query_map([], |r| {
                let sys_id = r.get::<_, i64>(0)? as SolarSystemId;
                let x: f64 = r.get(5)?;
                let y: f64 = r.get(6)?;
                let z: f64 = r.get(7)?;
                let pl_str: String = r.get(9)?;
                let planet_ids: Vec<i64> = parse_json_col(9, &pl_str)?;
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
                    stations: stations_by_system.remove(&sys_id).unwrap_or_default(),
                    wormhole_class_id: wh_class,
                })
            })?;
            for r in rows {
                let sys = r?;
                solar_systems.insert(sys.solar_system_id, sys);
            }
        }

        let mut jumps = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT stargate_id, system_id, destination_gate_id, destination_system_id, x, y, z FROM sde_stargates",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(StargateJump {
                    stargate_id: r.get(0)?,
                    from_solar_system_id: r.get::<_, i64>(1)? as SolarSystemId,
                    destination_stargate_id: r.get(2)?,
                    to_solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    position: DVec3::new(r.get(4)?, r.get(5)?, r.get(6)?),
                })
            })?;
            for r in rows {
                let j = r?;
                jumps.insert(j.stargate_id, j);
            }
        }

        let celestial_count: usize = conn
            .query_row("SELECT count(*) FROM sde_celestials", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize;

        let mut races = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT race_id, name, ship_type_id, skills_json, description FROM sde_races",
            )?;
            let rows = stmt.query_map([], |r| {
                let skills_json: Option<String> = r.get(3).ok();
                let starter_skills = skills_json
                    .and_then(|js| serde_json::from_str::<HashMap<String, u8>>(&js).ok())
                    .map(|m| {
                        m.into_iter()
                            .filter_map(|(k, v)| k.parse::<TypeId>().ok().map(|id| (id, v)))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(RaceInfo {
                    race_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    corvette_type_id: r.get::<_, Option<i64>>(2)?.map(|tid| tid as TypeId),
                    starter_skills,
                    description: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                })
            })?;
            for r in rows {
                let race = r?;
                races.insert(race.race_id, race);
            }
        }

        let mut bloodlines = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT bloodline_id, race_id, name, corporation_id, charisma, intelligence, memory, perception, willpower, ship_type_id, description FROM sde_bloodlines",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(BloodlineInfo {
                    bloodline_id: r.get::<_, i64>(0)? as u32,
                    race_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                    corporation_id: r.get(3)?,
                    charisma: r.get(4)?,
                    intelligence: r.get(5)?,
                    memory: r.get(6)?,
                    perception: r.get(7)?,
                    willpower: r.get(8)?,
                    ship_type_id: r.get::<_, Option<i64>>(9)?.map(|tid| tid as TypeId),
                    description: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                })
            })?;
            for r in rows {
                let b = r?;
                bloodlines.insert(b.bloodline_id, b);
            }
        }

        let mut ancestries = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT ancestry_id, bloodline_id, name, charisma, intelligence, memory, perception, willpower, description, short_description FROM sde_ancestries",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(AncestryInfo {
                    ancestry_id: r.get::<_, i64>(0)? as u32,
                    bloodline_id: r.get::<_, i64>(1)? as u32,
                    name: r.get(2)?,
                    charisma: r.get(3)?,
                    intelligence: r.get(4)?,
                    memory: r.get(5)?,
                    perception: r.get(6)?,
                    willpower: r.get(7)?,
                    description: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    short_description: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                })
            })?;
            for r in rows {
                let a = r?;
                ancestries.insert(a.ancestry_id, a);
            }
        }

        let mut schools = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT school_id, race_id, corporation_id, starting_station_id, starting_system_id, name FROM sde_schools",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SchoolInfo {
                    school_id: r.get::<_, i64>(0)? as u32,
                    race_id: r.get::<_, i64>(1)? as u32,
                    corporation_id: r.get(2)?,
                    station_id: r.get(3)?,
                    solar_system_id: r.get::<_, i64>(4)? as SolarSystemId,
                    name: r.get(5)?,
                })
            })?;
            for r in rows {
                let s = r?;
                schools.insert(s.school_id, s);
            }
        }

        let mut agents = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT agent_id, name, corporation_id, station_id, solar_system_id, level, division_id, agent_type_id, is_locator FROM sde_agents",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(AgentInfo {
                    agent_id: r.get(0)?,
                    name: r.get(1)?,
                    corporation_id: r.get(2)?,
                    station_id: r.get(3)?,
                    solar_system_id: r.get::<_, Option<i64>>(4)?.map(|v| v as SolarSystemId),
                    level: r.get::<_, i64>(5)? as u8,
                    division_id: r.get::<_, i64>(6)? as u32,
                    agent_type_id: r.get::<_, i64>(7)? as u32,
                    is_locator: r.get::<_, i64>(8)? != 0,
                })
            })?;
            for r in rows {
                let ag = r?;
                agents.insert(ag.agent_id, ag);
            }
        }

        let mut market_groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT market_group_id, parent_group_id, name, has_types, icon_id FROM sde_market_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MarketGroupInfo {
                    market_group_id: r.get::<_, i64>(0)? as u32,
                    parent_group_id: r.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    name: r.get(2)?,
                    has_types: r.get::<_, i64>(3)? != 0,
                    icon_id: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                })
            })?;
            for r in rows {
                let mg = r?;
                market_groups.insert(mg.market_group_id, mg);
            }
        }

        let mut planet_schematics = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT schematic_id, name, cycle_time, pins_json, types_json FROM sde_planet_schematics",
            )?;
            let rows = stmt.query_map([], |r| {
                let pins_str: String = r.get(3)?;
                let types_str: String = r.get(4)?;
                Ok(PlanetSchematicInfo {
                    schematic_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    cycle_time: r.get::<_, i64>(2)? as u64,
                    pins: parse_json_col(3, &pins_str)?,
                    types: parse_json_col(4, &types_str)?,
                })
            })?;
            for r in rows {
                let ps = r?;
                planet_schematics.insert(ps.schematic_id, ps);
            }
        }

        let mut wormhole_types = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_id, name, target_system_class, max_stable_time_min, max_stable_mass, mass_regeneration, max_jump_mass FROM sde_wormhole_types",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(WormholeTypeInfo {
                    type_id: r.get::<_, i64>(0)? as TypeId,
                    name: r.get(1)?,
                    target_system_class: r.get::<_, i64>(2)? as i32,
                    max_stable_time_min: r.get(3)?,
                    max_stable_mass: r.get(4)?,
                    mass_regeneration: r.get(5)?,
                    max_jump_mass: r.get(6)?,
                })
            })?;
            for r in rows {
                let wh = r?;
                wormhole_types.insert(wh.type_id, wh);
            }
        }

        let mut npc_spawn_groups = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT spawn_group_id, faction_id, ship_class, group_id, group_name, npc_type_ids_json FROM sde_npc_spawn_groups",
            )?;
            let rows = stmt.query_map([], |r| {
                let tids_str: String = r.get(5)?;
                Ok(NpcSpawnGroupInfo {
                    spawn_group_id: r.get::<_, i64>(0)? as u32,
                    faction_id: r.get::<_, i64>(1)? as u32,
                    ship_class: r.get::<_, i64>(2)? as u32,
                    group_id: r.get::<_, i64>(3)? as GroupId,
                    group_name: r.get(4)?,
                    npc_type_ids: parse_json_col(5, &tids_str)?,
                })
            })?;
            for r in rows {
                let sg = r?;
                npc_spawn_groups.insert(sg.spawn_group_id, sg);
            }
        }

        let mut clone_grades = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT grade_id, skills_json FROM sde_clone_grades")?;
            let rows = stmt.query_map([], |r| {
                let gid = r.get::<_, i64>(0)? as u32;
                let sk_str: String = r.get(1)?;
                let raw_caps: Vec<RawCloneSkillCap> = parse_json_col(1, &sk_str)?;
                let alpha_skill_caps: HashMap<TypeId, u8> =
                    raw_caps.into_iter().map(|c| (c.type_id, c.level)).collect();
                Ok(CloneGradeInfo {
                    grade_id: gid,
                    alpha_skill_caps,
                })
            })?;
            for r in rows {
                let cg = r?;
                clone_grades.insert(cg.grade_id, cg);
            }
        }

        let mut dbuff_collections = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT dbuff_id, operation_name, data_json FROM sde_dbuff_collections")?;
            let rows = stmt.query_map([], |r| {
                let id = r.get::<_, i64>(0)? as u32;
                let op: String = r.get(1)?;
                let d_str: String = r.get(2)?;
                let val: serde_json::Value = parse_json_col(2, &d_str)?;
                let name = val
                    .get("developerDescription")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("missing developerDescription on dbuff {id}").into(),
                        )
                    })?
                    .to_string();
                let agg = val
                    .get("aggregateMode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("missing aggregateMode on dbuff {id}").into(),
                        )
                    })?
                    .to_string();
                let display_name_id = val
                    .get("displayNameID")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let show_output_value_in_ui = val
                    .get("showOutputValueInUI")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let mut item_modifiers = Vec::new();
                if let Some(arr) = val.get("itemModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(aid) = item.get("dogmaAttributeID").and_then(|v| v.as_u64()) {
                            item_modifiers.push(aid as AttributeId);
                        }
                    }
                }
                let mut location_modifiers = Vec::new();
                if let Some(arr) = val.get("locationModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(aid) = item.get("dogmaAttributeID").and_then(|v| v.as_u64()) {
                            location_modifiers.push(aid as AttributeId);
                        }
                    }
                }
                let mut location_group_modifiers = Vec::new();
                if let Some(arr) = val.get("locationGroupModifiers").and_then(|v| v.as_array()) {
                    for item in arr {
                        if let (Some(aid), Some(gid)) = (
                            item.get("dogmaAttributeID").and_then(|v| v.as_u64()),
                            item.get("groupID").and_then(|v| v.as_u64()),
                        ) {
                            location_group_modifiers.push(DbuffGroupModifier {
                                attribute_id: aid as AttributeId,
                                group_id: gid as GroupId,
                            });
                        }
                    }
                }
                let mut location_required_skill_modifiers = Vec::new();
                if let Some(arr) = val
                    .get("locationRequiredSkillModifiers")
                    .and_then(|v| v.as_array())
                {
                    for item in arr {
                        if let (Some(aid), Some(sid)) = (
                            item.get("dogmaAttributeID").and_then(|v| v.as_u64()),
                            item.get("skillID").and_then(|v| v.as_u64()),
                        ) {
                            location_required_skill_modifiers.push(DbuffSkillModifier {
                                attribute_id: aid as AttributeId,
                                skill_id: sid as TypeId,
                            });
                        }
                    }
                }
                let mut target_attrs = Vec::new();
                target_attrs.extend(item_modifiers.iter().copied());
                target_attrs.extend(location_modifiers.iter().copied());
                target_attrs.extend(location_group_modifiers.iter().map(|m| m.attribute_id));
                target_attrs.extend(
                    location_required_skill_modifiers
                        .iter()
                        .map(|m| m.attribute_id),
                );
                Ok(DbuffCollectionInfo {
                    dbuff_id: id,
                    name,
                    operation: op,
                    aggregate_mode: agg,
                    display_name_id,
                    show_output_value_in_ui,
                    item_modifiers,
                    location_modifiers,
                    location_group_modifiers,
                    location_required_skill_modifiers,
                    target_attribute_ids: target_attrs,
                })
            })?;
            for r in rows {
                let d = r?;
                dbuff_collections.insert(d.dbuff_id, d);
            }
        }

        let mut fighter_abilities = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT ability_id, data_json FROM sde_fighter_abilities")?;
            let rows = stmt.query_map([], |r| {
                let id = r.get::<_, i64>(0)? as u32;
                let d_str: String = r.get(1)?;
                let val: serde_json::Value = parse_json_col(1, &d_str)?;
                let target_mode = val
                    .get("targetMode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            format!("missing targetMode on fighter ability {id}").into(),
                        )
                    })?
                    .to_string();
                let disallow_in_high_sec = val
                    .get("disallowInHighSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let disallow_in_low_sec = val
                    .get("disallowInLowSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let icon_id = val.get("iconID").and_then(|v| v.as_u64()).map(|v| v as u32);
                let display_name_id = val.get("displayNameID").and_then(|v| v.as_u64()).map(|v| v as u32);
                let ability_name_id = val
                    .get("abilityNameID")
                    .or_else(|| val.get("displayNameID"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let tooltip_text_id = val.get("tooltipTextID").and_then(|v| v.as_u64()).map(|v| v as u32);
                let turret_graphic_id = val.get("turretGraphicID").and_then(|v| v.as_u64()).map(|v| v as u32);
                Ok(FighterAbilityInfo {
                    ability_id: id,
                    target_mode,
                    disallow_in_high_sec,
                    disallow_in_low_sec,
                    icon_id,
                    display_name_id,
                    ability_name_id,
                    tooltip_text_id,
                    turret_graphic_id,
                    raw_json: val,
                })
            })?;
            for r in rows {
                let fa = r?;
                fighter_abilities.insert(fa.ability_id, fa);
            }
        }

        let mut compressible_types = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT type_id, compressed_type_id FROM sde_compressible_types")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)? as TypeId, r.get::<_, i64>(1)? as TypeId))
            })?;
            for r in rows {
                let (k, v) = r?;
                compressible_types.insert(k, v);
            }
        }

        let mut type_lists = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT type_list_id, name, included_category_ids_json, included_group_ids_json, included_type_ids_json, excluded_category_ids_json, excluded_group_ids_json, excluded_type_ids_json FROM sde_type_lists",
            )?;
            let parse_ids =
                |s: String| -> Vec<u32> { serde_json::from_str(&s).unwrap_or_default() };
            let rows = stmt.query_map([], |r| {
                Ok(TypeListInfo {
                    type_list_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    included_category_ids: parse_ids(r.get(2)?),
                    included_group_ids: parse_ids(r.get(3)?),
                    included_type_ids: parse_ids(r.get(4)?),
                    excluded_category_ids: parse_ids(r.get(5)?),
                    excluded_group_ids: parse_ids(r.get(6)?),
                    excluded_type_ids: parse_ids(r.get(7)?),
                })
            })?;
            for r in rows {
                let tl = r?;
                type_lists.insert(tl.type_list_id, tl);
            }
        }

        let mut dungeon_asteroids: HashMap<u32, Vec<DungeonAsteroidEntry>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT dungeon_id, room_id, type_id, asteroid_count FROM sde_dungeon_asteroid_counts ORDER BY dungeon_id, room_id, type_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u32,
                    DungeonAsteroidEntry {
                        room_id: r.get::<_, i64>(1)? as u32,
                        type_id: r.get::<_, i64>(2)? as TypeId,
                        asteroid_count: r.get::<_, i64>(3)? as u32,
                    },
                ))
            })?;
            for r in rows {
                let (did, entry) = r?;
                dungeon_asteroids.entry(did).or_default().push(entry);
            }
        }

        let mut dungeon_gas_types: HashMap<u32, Vec<TypeId>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT dungeon_id, type_id FROM sde_dungeon_gas_types ORDER BY dungeon_id, type_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as TypeId))
            })?;
            for r in rows {
                let (did, tid) = r?;
                dungeon_gas_types.entry(did).or_default().push(tid);
            }
        }

        let mut dynamic_item_attributes = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT mutaplasmid_type_id, mappings_json, attribute_ranges_json FROM sde_dynamic_item_attributes",
            )?;
            let rows = stmt.query_map([], |r| {
                let mid = r.get::<_, i64>(0)? as TypeId;
                let map_str: String = r.get(1)?;
                let rng_str: String = r.get(2)?;
                let mappings: Vec<RawMutaMapping> = parse_json_col(1, &map_str)?;
                let raw_rng: HashMap<String, [f64; 2]> = parse_json_col(2, &rng_str)?;
                let mut attribute_ranges = HashMap::new();
                for (k, v) in raw_rng {
                    if let Ok(aid) = k.parse::<u16>() {
                        attribute_ranges.insert(aid, v);
                    }
                }
                let resulting_type_id =
                    mappings.first().map(|m| m.resulting_type_id).unwrap_or(mid);
                let applicable_type_ids: Vec<TypeId> = mappings
                    .into_iter()
                    .flat_map(|m| m.applicable_type_ids)
                    .collect();
                Ok(MutaplasmidInfo {
                    mutaplasmid_type_id: mid,
                    applicable_type_ids,
                    resulting_type_id,
                    attribute_ranges,
                })
            })?;
            for r in rows {
                let m = r?;
                dynamic_item_attributes.insert(m.mutaplasmid_type_id, m);
            }
        }

        let mut space_components_by_type = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT type_id, components_json FROM sde_space_components")?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let c_str: String = r.get(1)?;
                let comps: Vec<String> = parse_json_col(1, &c_str)?;
                Ok((tid, comps))
            })?;
            for r in rows {
                let (tid, comps) = r?;
                space_components_by_type.insert(tid, comps);
            }
        }

        let mut missions = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT mission_id, name, is_courier, lp_alpha, lp_omega, has_standing_rewards, content_template FROM sde_missions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(MissionTemplate {
                    mission_id: r.get::<_, i64>(0)? as u32,
                    title: r.get(1)?,
                    is_courier: r.get::<_, i64>(2)? != 0,
                    lp_alpha: r.get(3)?,
                    lp_omega: r.get(4)?,
                    has_standing_rewards: r.get::<_, i64>(5)? != 0,
                    content_template: r.get(6)?,
                })
            })?;
            for r in rows {
                let m = r?;
                missions.insert(m.mission_id, m);
            }
        }

        let mut dungeons = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT dungeon_id, name, archetype_id, faction_id, difficulty FROM sde_dungeons",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DungeonTemplate {
                    dungeon_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    archetype_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    faction_id: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    difficulty: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                })
            })?;
            for r in rows {
                let d = r?;
                dungeons.insert(d.dungeon_id, d);
            }
        }

        let mut epic_arcs = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT epic_arc_id, name, faction_id, restart_interval, mission_count FROM sde_epic_arcs",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(EpicArcTemplate {
                    epic_arc_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    faction_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    restart_interval: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    missions_count: r.get::<_, i64>(4)?.max(0) as u32,
                })
            })?;
            for r in rows {
                let ea = r?;
                epic_arcs.insert(ea.epic_arc_id, ea);
            }
        }

        let mut node_graphs = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT node_graph_id, name, parent_graph_id, node_count, tags_json FROM sde_node_graphs",
            )?;
            let rows = stmt.query_map([], |r| {
                let tags_str: String = r.get(4)?;
                Ok(NodeGraphTemplate {
                    node_graph_id: r.get::<_, i64>(0)? as u32,
                    name: r.get(1)?,
                    parent_graph_id: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    node_count: r.get::<_, i64>(3)?.max(0) as u32,
                    tags: parse_json_col(4, &tags_str)?,
                })
            })?;
            for r in rows {
                let ng = r?;
                node_graphs.insert(ng.node_graph_id, ng);
            }
        }

        let mut fighter_abilities_by_type = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT type_id, data_json FROM sde_fighter_abilities_by_type")?;
            let rows = stmt.query_map([], |r| {
                let tid = r.get::<_, i64>(0)? as TypeId;
                let d_str: String = r.get(1)?;
                let val: serde_json::Value = parse_json_col(1, &d_str)?;
                let mut slots = Vec::new();
                if let Some(obj) = val.as_object() {
                    for (slot_idx, slot_key) in ["abilitySlot0", "abilitySlot1", "abilitySlot2"]
                        .into_iter()
                        .enumerate()
                    {
                        if let Some(slot_obj) = obj.get(slot_key).and_then(|s| s.as_object()) {
                            if let Some(aid) = slot_obj.get("abilityID").and_then(|v| v.as_u64()) {
                                let cooldown_seconds =
                                    slot_obj.get("cooldownSeconds").and_then(|v| v.as_f64());
                                let charges = slot_obj
                                    .get("charges")
                                    .and_then(|c| c.as_object())
                                    .and_then(|c| {
                                        Some(FighterAbilityCharges {
                                            charge_count: c.get("chargeCount")?.as_u64()? as u32,
                                            rearm_time_seconds: c
                                                .get("rearmTimeSeconds")?
                                                .as_f64()?,
                                        })
                                    });
                                slots.push(FighterAbilitySlot {
                                    slot_index: slot_idx as u8,
                                    ability_id: aid as u32,
                                    cooldown_seconds,
                                    charges,
                                });
                            }
                        }
                    }
                }
                Ok((tid, slots))
            })?;
            for r in rows {
                let (tid, slots) = r?;
                fighter_abilities_by_type.insert(tid, slots);
            }
        }

        let (skin_licenses, skin_materials, skins) =
            crate::sqlite_loader_skins::load_skin_tables(&conn)?;

        let registry = Self {
            db_path: Some(canon),
            resources,
            categories,
            groups,
            types,
            variations_by_parent,
            attributes,
            effects,
            races,
            bloodlines,
            ancestries,
            schools,
            regions,
            constellations,
            solar_systems,
            stations,
            jumps,
            celestial_count,
            npc_factions: factions.clone(),
            factions,
            npc_corporations,
            agents,
            market_groups,
            planet_schematics,
            wormhole_types,
            npc_spawn_groups,
            clone_grades,
            blueprints,
            dbuff_collections,
            fighter_abilities,
            fighter_abilities_by_type,
            wrecks,
            compressible_types,
            type_lists,
            dungeon_asteroids,
            dungeon_gas_types,
            type_materials,
            randomized_type_materials,
            dynamic_item_attributes,
            space_components_by_type,
            missions,
            dungeons,
            epic_arcs,
            node_graphs,
            career_goals: HashMap::new(),
            station_operations,
            station_services,
            skin_licenses,
            skin_materials,
            skins,
        };

        Ok(registry)
    }

    /// Loads the runtime SQLite SDE database at [`Self::default_db_path`] (`data/evomu_sde.sqlite`,
    /// overridable via `EVOMU_SDE_PATH`), failing loudly if it is missing (`D-2`).
    pub fn with_canonical_seed() -> Self {
        let p = Self::default_db_path();
        Self::open(&p).unwrap_or_else(|e| {
            panic!(
                "Failed to open runtime SDE SQLite database at '{}': {e}",
                p.display()
            )
        })
    }
}
