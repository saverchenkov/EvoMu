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
}
