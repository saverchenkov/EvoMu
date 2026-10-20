//! `mining_sites` — data model for cosmic ore anomalies, hidden ore signatures, gas sites and
//! Sovereignty Hub prospecting deposits.
//!
//! Sources, per site:
//! - **Which asteroids, and how many of each type**: the client's
//!   `res:/staticdata/asteroidcountbytypeidroomiddungeonid.static` (imported as
//!   `sde_dungeon_asteroid_counts` → [`eve_sde::SdeRegistry::dungeon_asteroids`]).
//! - **Which gas cloud type a gas site contains**: the client's `res:/staticdata/gastypesbydungeonid.static`
//!   (`sde_dungeon_gas_types` → [`eve_sde::SdeRegistry::dungeon_gas_types`]).
//! - **How many units each asteroid / cloud holds and where a site spawns**: not in client static data
//!   (server-side). These are transcribed from the EVE University wiki rows cited next to each entry.
//!
//! Nothing here falls back silently: a site whose client layout and wiki quantities don't line up
//! is reported as an error and isn't spawned.

#![forbid(unsafe_code)]

use eve_sde::{SdeRegistry, TypeId};

/// Rounds a system's true security to the displayed security level: values strictly between
/// `0.0` and `0.05` round up to `0.1`, everything else rounds to one decimal
/// (`eve-wiki/articles/System_security.md:190-193`).
pub fn displayed_security(true_sec: f64) -> f64 {
    if true_sec > 0.0 && true_sec < 0.05 {
        0.1
    } else {
        (true_sec * 10.0).round() / 10.0
    }
}

/// Security band in which a site spawns, evaluated on the displayed security level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteBand {
    /// Displayed `0.5..=0.8` (`eve-wiki/articles/Asteroids_and_ore.md:166-167`).
    HighSecMid,
    /// Displayed `0.5..=1.0`.
    HighSec,
    /// Displayed `0.1..=0.4`.
    LowSec,
    /// Displayed `<= 0.0`, known space only.
    NullSec,
}

impl SiteBand {
    pub fn matches(self, solar_system_id: u32, true_sec: f64) -> bool {
        if solar_system_id >= 31_000_000 {
            return false;
        }
        let d = displayed_security(true_sec);
        match self {
            SiteBand::HighSecMid => (0.5..=0.8).contains(&d),
            SiteBand::HighSec => d >= 0.5,
            SiteBand::LowSec => (0.1..=0.4).contains(&d),
            SiteBand::NullSec => d <= 0.0,
        }
    }
}

/// Whether a site is visible on the overview immediately (anomaly) or must be probed (signature).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    Anomaly,
    Signature,
}

/// Selects the client asteroids a wiki quantity row applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OreSelector {
    /// One exact asteroid type (wiki rows that list each grade separately).
    Type(TypeId),
    /// Every asteroid of an SDE ore group (wiki rows that sum all grades of one ore).
    Group(u32),
}

/// One wiki quantity row: `total_units` of ore spread across every client asteroid matched by `selector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OreQuantityRow {
    pub selector: OreSelector,
    pub total_units: i64,
}

/// A cosmic ore site whose asteroid layout comes from the client and whose quantities come from the wiki.
#[derive(Debug, Clone, Copy)]
pub struct OreSiteDef {
    /// Exact `sde_dungeons.name` (matched after trimming whitespace).
    pub dungeon_name: &'static str,
    pub kind: SiteKind,
    pub band: SiteBand,
    pub rows: &'static [OreQuantityRow],
    /// Wiki rows the quantities were transcribed from.
    pub source: &'static str,
}

const fn t(type_id: TypeId, total_units: i64) -> OreQuantityRow {
    OreQuantityRow {
        selector: OreSelector::Type(type_id),
        total_units,
    }
}

const fn g(group_id: u32, total_units: i64) -> OreQuantityRow {
    OreQuantityRow {
        selector: OreSelector::Group(group_id),
        total_units,
    }
}

/// Cosmic ore anomalies and hidden ore signatures seeded per security band. Old ore variant names on
/// the wiki map to the Catalyst grade names as +5% → II-Grade, +10% → III-Grade, +15% → IV-Grade
/// (`eve-wiki/articles/Asteroids_and_ore.md:389-405`).
pub const COSMIC_ORE_SITES: &[OreSiteDef] = &[
    OreSiteDef {
        dungeon_name: "Small Kernite and Omber Deposit",
        kind: SiteKind::Anomaly,
        band: SiteBand::HighSecMid,
        rows: &[
            t(17453, 11_811), // Kernite III-Grade
            t(17452, 16_965), // Kernite II-Grade
            t(20, 29_726),    // Kernite
            t(17868, 12_921), // Omber III-Grade
            t(17867, 5_754),  // Omber II-Grade
            t(1227, 47_013),  // Omber
        ],
        source: "eve-wiki/articles/Small_Kernite_and_Omber_Deposit.md:17-22 (Site Total (units) column)",
    },
    OreSiteDef {
        dungeon_name: "Hidden Omber Deposit",
        kind: SiteKind::Signature,
        band: SiteBand::HighSec,
        rows: &[
            t(17867, 314_233), // Omber II-Grade
            t(17868, 316_096), // Omber III-Grade
            t(46684, 561_268), // Omber IV-Grade
        ],
        source: "eve-wiki/articles/Hidden_Omber_Deposit.md:19-21 (Ore Quantity column)",
    },
    OreSiteDef {
        dungeon_name: "Average Hedbergite, Hemorphite and Jaspet Deposit",
        kind: SiteKind::Anomaly,
        band: SiteBand::LowSec,
        rows: &[
            t(21, 30_000),    // Hedbergite
            t(17440, 26_000), // Vitric Hedbergite = Hedbergite II-Grade
            t(17441, 18_000), // Glazed Hedbergite = Hedbergite III-Grade
            t(17444, 26_000), // Vivid Hemorphite = Hemorphite II-Grade
            t(17445, 38_000), // Radiant Hemorphite = Hemorphite III-Grade
            t(1226, 36_000),  // Jaspet
            t(17448, 22_000), // Pure Jaspet = Jaspet II-Grade
            t(17449, 24_000), // Pristine Jaspet = Jaspet III-Grade
        ],
        source: "eve-wiki/articles/Average_Hedbergite,_Hemorphite_and_Jaspet_Deposit.md:20-27 (Ore Quantity column)",
    },
    OreSiteDef {
        dungeon_name: "Hidden Crokite, Dark Ochre and Gneiss Deposit",
        kind: SiteKind::Signature,
        band: SiteBand::LowSec,
        rows: &[
            t(17432, 180_000),                // Crokite II-Grade
            t(46677, 160_000),                // Crokite IV-Grade
            t(17436, 4_800 + 5_600 + 6_400),  // Dark Ochre II-Grade (3 asteroids)
            t(46675, 6_400),                  // Dark Ochre IV-Grade
            t(17865, 2_000 + 4_000 + 6_400 + 8_800), // Gneiss II-Grade (4 asteroids)
            t(17866, 3_200 + 4_000 + 6_400),  // Gneiss III-Grade (3 asteroids)
            t(46679, 4_800),                  // Gneiss IV-Grade
        ],
        source: "eve-wiki/articles/Hidden_Crokite,_Dark_Ochre_and_Gneiss_Deposit.md:18-31 (Quantity column)",
    },
    OreSiteDef {
        dungeon_name: "Small Arkonor and Bistot Deposit",
        kind: SiteKind::Anomaly,
        band: SiteBand::NullSec,
        rows: &[
            g(450, 7_000), // Arkonor
            g(451, 7_000), // Bistot
        ],
        source: "eve-wiki/articles/Small_Arkonor_and_Bistot_Deposit.md:26-28 (Ore Quantity column); 224k m3 total at Asteroids_and_ore.md:184",
    },
];

/// A Sovereignty Hub prospecting upgrade and the deposit it spawns.
#[derive(Debug, Clone, Copy)]
pub struct SovDepositDef {
    /// SDE type of the group-4768 prospecting upgrade.
    pub upgrade_type_id: TypeId,
    /// Upgrade level as listed on the wiki next to the deposit it spawns.
    pub level: u8,
    pub site: OreSiteDef,
}

/// Prospecting upgrades whose deposits are modelled. Upgrade → deposit mapping:
/// `eve-wiki/articles/Asteroids_and_ore.md:239-242`. Other prospecting upgrades are rejected with
/// `ProspectingDepositNotModelled`.
pub const SOV_PROSPECTING_DEPOSITS: &[SovDepositDef] = &[SovDepositDef {
    upgrade_type_id: 87702, // Tritanium Prospecting Array 3
    level: 3,
    site: OreSiteDef {
        dungeon_name: "Large Veldspar Deposit",
        kind: SiteKind::Anomaly,
        band: SiteBand::NullSec,
        rows: &[
            g(458, 4_614_271),   // Plagioclase (all grades)
            g(462, 114_769_310), // Veldspar (all grades)
            g(460, 14_166_663),  // Scordite (all grades)
        ],
        // The wiki lists 27 Veldspar asteroids; the V24.01 client layout has 31, so the wiki total is
        // spread over the client's asteroids.
        source: "eve-wiki/articles/Large_Veldspar_Deposit.md:19-21 (Ore Quantity column)",
    },
}];

/// A gas site: cloud type from the client, per-cloud units and region from the wiki infobox.
#[derive(Debug, Clone, Copy)]
pub struct GasSiteDef {
    pub dungeon_name: &'static str,
    /// SDE region names the site spawns in.
    pub region_names: &'static [&'static str],
    pub band: SiteBand,
    pub cloud_units: &'static [i64],
    pub source: &'static str,
}

/// Low-sec Cytoserocin gas sites whose wiki infobox names both the region and the security band.
pub const GAS_SITES: &[GasSiteDef] = &[
    GasSiteDef {
        dungeon_name: "Saintly Nebula",
        region_names: &["Heimatar"],
        band: SiteBand::LowSec,
        cloud_units: &[200, 300],
        source: "eve-wiki/articles/Saintly_Nebula.md:17-18",
    },
    GasSiteDef {
        dungeon_name: "Bandit Nebula",
        region_names: &["The Forge"],
        band: SiteBand::LowSec,
        cloud_units: &[200, 300],
        source: "eve-wiki/articles/Bandit_Nebula.md:16-17",
    },
];

/// One asteroid to spawn: its type and unit quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteAsteroid {
    pub type_id: TypeId,
    pub units: i64,
}

/// A fully resolved ore site, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOreSite {
    pub dungeon_id: u32,
    pub name: String,
    pub asteroids: Vec<SiteAsteroid>,
}

impl ResolvedOreSite {
    /// Distinct ore types in the site, ascending.
    pub fn ore_types(&self) -> Vec<TypeId> {
        let mut v: Vec<TypeId> = self.asteroids.iter().map(|a| a.type_id).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Finds a dungeon by exact name (trimmed, since some client names carry trailing spaces).
pub fn find_dungeon_id(sde: &SdeRegistry, name: &str) -> Option<u32> {
    let mut ids: Vec<u32> = sde
        .dungeons
        .values()
        .filter(|d| d.name.trim() == name)
        .map(|d| d.dungeon_id)
        .collect();
    ids.sort_unstable();
    ids.first().copied()
}

/// Resolves `def` against the client asteroid layout. Every client asteroid must be matched by
/// exactly one wiki row and every wiki row must match at least one asteroid; otherwise an error is
/// returned and the site must not be spawned. A row's total is split evenly over its asteroids, with
/// the remainder going to the first ones (the wiki gives per-ore totals, not per-asteroid sizes).
pub fn resolve_ore_site(sde: &SdeRegistry, def: &OreSiteDef) -> Result<ResolvedOreSite, String> {
    let dungeon_id = find_dungeon_id(sde, def.dungeon_name)
        .ok_or_else(|| format!("dungeon '{}' not found in sde_dungeons", def.dungeon_name))?;
    let layout = sde
        .dungeon_asteroids
        .get(&dungeon_id)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("dungeon {dungeon_id} has no client asteroid layout"))?;

    let row_matches = |row: &OreQuantityRow, type_id: TypeId| -> bool {
        match row.selector {
            OreSelector::Type(tid) => tid == type_id,
            OreSelector::Group(gid) => sde.types.get(&type_id).is_some_and(|ty| ty.group_id == gid),
        }
    };

    let mut per_row_types: Vec<Vec<(TypeId, u32)>> = vec![Vec::new(); def.rows.len()];
    for entry in layout {
        let matching: Vec<usize> = def
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row_matches(row, entry.type_id))
            .map(|(i, _)| i)
            .collect();
        match matching.as_slice() {
            [i] => per_row_types[*i].push((entry.type_id, entry.asteroid_count)),
            [] => {
                return Err(format!(
                    "dungeon {dungeon_id} asteroid type {} has no wiki quantity row ({})",
                    entry.type_id, def.source
                ))
            }
            _ => {
                return Err(format!(
                    "dungeon {dungeon_id} asteroid type {} matches several wiki rows",
                    entry.type_id
                ))
            }
        }
    }

    let mut asteroids = Vec::new();
    for (row, types) in def.rows.iter().zip(per_row_types.iter()) {
        let count: i64 = types.iter().map(|&(_, c)| i64::from(c)).sum();
        if count == 0 {
            return Err(format!(
                "wiki row {:?} matches no asteroid in dungeon {dungeon_id}",
                row.selector
            ));
        }
        let base = row.total_units / count;
        let mut remainder = row.total_units % count;
        for &(type_id, c) in types {
            for _ in 0..c {
                let extra = if remainder > 0 {
                    remainder -= 1;
                    1
                } else {
                    0
                };
                asteroids.push(SiteAsteroid {
                    type_id,
                    units: base + extra,
                });
            }
        }
    }
    if asteroids.iter().any(|a| a.units <= 0) {
        return Err(format!(
            "dungeon {dungeon_id} resolves an asteroid with no ore"
        ));
    }

    Ok(ResolvedOreSite {
        dungeon_id,
        name: def.dungeon_name.to_string(),
        asteroids,
    })
}

/// A fully resolved gas site, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGasSite {
    pub dungeon_id: u32,
    pub name: String,
    pub gas_type_id: TypeId,
    pub cloud_units: Vec<i64>,
}

/// Resolves a gas site's cloud type from the client gas layout. The site must list exactly one gas type.
pub fn resolve_gas_site(sde: &SdeRegistry, def: &GasSiteDef) -> Result<ResolvedGasSite, String> {
    let dungeon_id = find_dungeon_id(sde, def.dungeon_name)
        .ok_or_else(|| format!("dungeon '{}' not found in sde_dungeons", def.dungeon_name))?;
    let gas_types = sde
        .dungeon_gas_types
        .get(&dungeon_id)
        .ok_or_else(|| format!("dungeon {dungeon_id} has no client gas layout"))?;
    let [gas_type_id] = gas_types.as_slice() else {
        return Err(format!(
            "dungeon {dungeon_id} lists {} gas types; expected exactly one",
            gas_types.len()
        ));
    };
    Ok(ResolvedGasSite {
        dungeon_id,
        name: def.dungeon_name.to_string(),
        gas_type_id: *gas_type_id,
        cloud_units: def.cloud_units.to_vec(),
    })
}

/// Region name of a solar system, from the SDE.
pub fn region_name(sde: &SdeRegistry, solar_system_id: u32) -> Option<&str> {
    let region_id = sde.solar_systems.get(&solar_system_id)?.region_id;
    sde.regions.get(&region_id).map(|r| r.name.as_str())
}

/// Position of the `index`-th asteroid of a site relative to the site centre. DERIVED: client static
/// data holds no in-site asteroid coordinates; asteroids are laid out on a flat golden-angle spiral at
/// 1.5 km + 250 m × index, which keeps every asteroid of the largest modelled site (31 asteroids)
/// within 9 km of the site centre.
pub fn asteroid_offset(index: usize) -> [f64; 3] {
    const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;
    let r = 1_500.0 + 250.0 * index as f64;
    let a = GOLDEN_ANGLE * index as f64;
    [r * a.cos(), 0.0, r * a.sin()]
}
