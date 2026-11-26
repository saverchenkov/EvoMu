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

use crate::router::ServerContext;
pub use crate::services::sov_fw_insurgency::derive_system_site_position_from_sde;
use crate::system_actor::AsteroidState;
use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, TypeId};
use glam::DVec3;
use std::collections::HashMap;
use tracing::warn;

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

    /// Checks whether `sys` matches this security band, excluding Pochven (`region_id == 10_000_070`,
    /// Triglavian space, which does not spawn standard empire or null-sec cosmic ore/gas sites).
    pub fn matches_system(self, sys: &eve_sde::SolarSystem) -> bool {
        if sys.region_id == 10_000_070 {
            return false;
        }
        self.matches(sys.solar_system_id, sys.security_status)
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
    /// Respawn time in seconds after the deposit is cleared (`eve-wiki/articles/Asteroids_and_ore.md:239-242`).
    pub respawn_seconds: u64,
    pub site: OreSiteDef,
}

/// Prospecting upgrades whose deposits are modelled. Upgrade → deposit mapping:
/// `eve-wiki/articles/Asteroids_and_ore.md:239-242`. Other prospecting upgrades are rejected with
/// `ProspectingDepositNotModelled`.
pub const SOV_PROSPECTING_DEPOSITS: &[SovDepositDef] = &[
    SovDepositDef {
        upgrade_type_id: 87702, // Tritanium Prospecting Array 3
        level: 3,
        respawn_seconds: 36_000, // 10 hours respawn time (eve-wiki/articles/Asteroids_and_ore.md:242)
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
    },
    SovDepositDef {
        upgrade_type_id: 82589, // Pyerite Prospecting Array 1
        level: 1,
        respawn_seconds: 36_000,
        site: OreSiteDef {
            dungeon_name: "Small Mordunium Deposit",
            kind: SiteKind::Anomaly,
            band: SiteBand::NullSec,
            rows: &[
                g(459, 2_200_000),  // Pyroxeres (all grades)
                g(460, 3_900_000),  // Scordite (all grades)
                g(4513, 6_900_000), // Mordunium (all grades)
            ],
            source: "eve-wiki/articles/Small_Mordunium_Deposit.md (Ore Quantity column)",
        },
    },
    SovDepositDef {
        upgrade_type_id: 82587, // Megacyte Prospecting Array 1
        level: 1,
        respawn_seconds: 36_000,
        site: OreSiteDef {
            dungeon_name: "Small Ueganite Deposit",
            kind: SiteKind::Anomaly,
            band: SiteBand::NullSec,
            rows: &[
                g(450, 250_000),  // Arkonor (all grades)
                g(4757, 500_000), // Ueganite (all grades)
            ],
            source: "eve-wiki/articles/Small_Ueganite_Deposit.md (Ore Quantity column)",
        },
    },
];

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

/// Respawn delay (`14,400`s = 4 hours) after the last ice asteroid in an ice belt anomaly is depleted
/// (`eve-wiki/articles/Ice_harvesting.md:62-66`).
pub const ICE_BELT_RESPAWN_DELAY_SECS: u64 = 14_400;

/// Natural expiry delay (`259,200`s = 72 hours = 3 days) after which an unmined or partially mined
/// cosmic ore anomaly despawns (`eve-wiki/articles/Cosmic_anomaly.md`).
pub const ORE_ANOMALY_EXPIRY_SECS: u64 = 3 * 86_400;

/// Returns whether a cosmic ore anomaly of age `age_secs` has reached its natural timed expiry.
pub const fn is_ore_anomaly_expired(age_secs: u64) -> bool {
    age_secs >= ORE_ANOMALY_EXPIRY_SECS
}

/// Returns whether a depleted ice belt anomaly is ready to respawn after `elapsed_secs`.
pub const fn is_ice_belt_ready_to_respawn(elapsed_secs: u64) -> bool {
    elapsed_secs >= ICE_BELT_RESPAWN_DELAY_SECS
}

/// Returns whether `solar_system_id` is a designated ice-belt solar system (`plan:MN-08`).
pub const fn is_designated_ice_system(solar_system_id: u32) -> bool {
    matches!(
        solar_system_id,
        30_000_180 | 30_000_156 | 30_002_510 | 30_002_659 | 30_003_495 | 30_000_192
    )
}

/// Regional Ice Belt anomaly definition (`sde_dungeons` + `sde_dungeon_asteroid_counts`).
#[derive(Debug, Clone, Copy)]
pub struct IceBeltSiteDef {
    pub region_names: &'static [&'static str],
    pub site: OreSiteDef,
}

/// Empire & Low-Sec Regional Ice Belt cosmic anomalies (`feat:mining.ice-belts`).
pub const ICE_BELT_SITES: &[IceBeltSiteDef] = &[
    IceBeltSiteDef {
        region_names: &["The Forge", "Lonetrek", "The Citadel", "Black Rise"],
        site: OreSiteDef {
            dungeon_name: "Blue Ice Belt",
            kind: SiteKind::Anomaly,
            band: SiteBand::HighSec,
            rows: &[t(16264, 21_000)], // 21 Blue Ice asteroids (1,000 units each)
            source: "sde_dungeon_asteroid_counts dungeon 5496 (21x Blue Ice 16264)",
        },
    },
    IceBeltSiteDef {
        region_names: &["Sinq Laison", "Everyshore", "Essence", "Verge Vendor"],
        site: OreSiteDef {
            dungeon_name: "White Glaze Belt",
            kind: SiteKind::Anomaly,
            band: SiteBand::HighSec,
            rows: &[t(16265, 19_000)], // 19 White Glaze asteroids
            source: "sde_dungeon_asteroid_counts dungeon 5497 (19x White Glaze 16265)",
        },
    },
    IceBeltSiteDef {
        region_names: &["Heimatar", "Metropolis", "Molden Heath"],
        site: OreSiteDef {
            dungeon_name: "Glacial Mass Belt",
            kind: SiteKind::Anomaly,
            band: SiteBand::HighSec,
            rows: &[t(16263, 20_000)], // 20 Glacial Mass asteroids
            source: "sde_dungeon_asteroid_counts dungeon 5498 (20x Glacial Mass 16263)",
        },
    },
    IceBeltSiteDef {
        region_names: &["Domain", "Kor-Azor", "Tash-Murkon", "Kador", "Khanid"],
        site: OreSiteDef {
            dungeon_name: "Clear Icicle Belt",
            kind: SiteKind::Anomaly,
            band: SiteBand::HighSec,
            rows: &[t(16262, 19_000)], // 19 Clear Icicle asteroids
            source: "sde_dungeon_asteroid_counts dungeon 5499 (19x Clear Icicle 16262)",
        },
    },
    IceBeltSiteDef {
        region_names: &["The Forge", "Black Rise", "Heimatar", "Domain", "Sinq Laison"],
        site: OreSiteDef {
            dungeon_name: "Thick Blue Ice Belt",
            kind: SiteKind::Anomaly,
            band: SiteBand::LowSec,
            rows: &[
                t(16266, 6_000),  // 6 Glare Crust
                t(16267, 7_000),  // 7 Dark Glitter
                t(16268, 4_000),  // 4 Gelidus
                t(17975, 20_000), // 20 Blue Ice IV-Grade (Pristine White Glaze / Thick Blue Ice)
            ],
            source: "sde_dungeon_asteroid_counts dungeon 5504 (Low-Sec Ice Belt)",
        },
    },
];

/// Resolves the regional Ice Belt anomaly for `solar_system_id` from SDE `sde_dungeon_asteroid_counts`.
pub fn resolve_regional_ice_belt(
    sde: &SdeRegistry,
    solar_system_id: u32,
) -> Option<Result<ResolvedOreSite, String>> {
    let sys = sde.solar_systems.get(&solar_system_id)?;
    let reg = region_name(sde, solar_system_id)?;
    let def = ICE_BELT_SITES.iter().find(|d| {
        d.site.band.matches(solar_system_id, sys.security_status) && d.region_names.contains(&reg)
    })?;
    Some(resolve_ore_site(sde, &def.site))
}

/// Guarded Wormhole Fullerite gas site (`sde_dungeon_gas_types` + Sleeper NPC guard wave).
#[derive(Debug, Clone, Copy)]
pub struct WormholeGasSiteDef {
    pub dungeon_name: &'static str,
    pub clouds: &'static [(TypeId, i64)],
    pub guard_npc_type_id: TypeId,
    pub guard_npc_count: u32,
    pub guard_spawn_delay_secs: u64,
    pub source: &'static str,
}

/// Guarded Wormhole Fullerite gas reservoirs (`feat:mining.gas-sites`, `feat:mining.gas-harvesting`).
pub const WORMHOLE_GAS_SITES: &[WormholeGasSiteDef] = &[
    WormholeGasSiteDef {
        dungeon_name: "Barren Perimeter Reservoir",
        clouds: &[(30370, 3_000), (30371, 1_500)], // Fullerite-C50, Fullerite-C60
        guard_npc_type_id: 30209,                  // Emergent Patroller (Sleeper Frigate)
        guard_npc_count: 5,
        guard_spawn_delay_secs: 900,
        source: "eve-wiki/articles/Barren_Perimeter_Reservoir.md (Fullerite-C50/C60 + 5x Emergent Patroller)",
    },
    WormholeGasSiteDef {
        dungeon_name: "Bountiful Frontier Reservoir",
        clouds: &[(30375, 5_000), (30376, 1_000)], // Fullerite-C28, Fullerite-C32
        guard_npc_type_id: 30210,                  // Emergent Watchman (Sleeper Frigate)
        guard_npc_count: 6,
        guard_spawn_delay_secs: 900,
        source: "eve-wiki/articles/Bountiful_Frontier_Reservoir.md (Fullerite-C28/C32 + 6x Emergent Watchman)",
    },
];

/// Resolves a guarded wormhole gas site against `sde_dungeon_gas_types`.
pub fn resolve_wormhole_gas_site(
    sde: &SdeRegistry,
    def: &WormholeGasSiteDef,
) -> Result<(u32, Vec<(TypeId, i64)>), String> {
    let dungeon_id = find_dungeon_id(sde, def.dungeon_name)
        .ok_or_else(|| format!("dungeon '{}' not found in sde_dungeons", def.dungeon_name))?;
    let gas_types = sde
        .dungeon_gas_types
        .get(&dungeon_id)
        .ok_or_else(|| format!("dungeon {dungeon_id} has no client gas layout"))?;
    for &(tid, _) in def.clouds {
        if !gas_types.contains(&tid) {
            return Err(format!(
                "dungeon {dungeon_id} client gas layout {gas_types:?} missing cloud type {tid}"
            ));
        }
    }
    Ok((dungeon_id, def.clouds.to_vec()))
}

pub use super::space_mining_harvest::{
    PhasedAsteroidFieldState, WORMHOLE_GUARD_TYPE_IDS, WORMHOLE_ORE_SITES, WhOreAnomalyGuardState,
    WormholeOreSiteDef, build_wormhole_guard_wave, collect_persisted_wormhole_guard_sites,
    hydrate_wormhole_ore_guards_if_needed, on_wormhole_ore_guard_destroyed_db,
    record_initial_wormhole_guard_wave, resolve_wormhole_ore_site,
    resolve_wormhole_ore_site_with_dungeon, respawn_wormhole_ore_guards_if_missing,
    sample_wormhole_ore_sites_for_system, spawn_seeded_site_entities, spawn_wormhole_ore_guards,
    wormhole_ore_sites_for_class,
};

use crate::services::sov_fw_insurgency::derive_system_site_position;

fn type_vol_rad_from_sde(sde: &SdeRegistry, tid: u32) -> (f64, f64) {
    sde.types
        .get(&tid)
        .map(|t| (t.volume.max(0.001), t.radius.max(50.0)))
        .unwrap_or((0.001, 50.0))
}

fn collect_sov_asteroid_ids(db: &eve_db::WorldRepository, sys_id: u32) -> Option<Vec<i64>> {
    db.with_snapshot(|snap| {
        let has_sov_anom = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre"));
        if !has_sov_anom {
            return None;
        }
        Some(
            snap.asteroids
                .values()
                .filter(|a| a.solar_system_id == sys_id && a.belt_id == 0)
                .map(|a| a.asteroid_id)
                .collect(),
        )
    })
}

fn build_sov_asteroid_ball_and_state(
    aid: i64,
    tid: TypeId,
    units: u64,
    uvol: f64,
    rad: f64,
    pos: DVec3,
) -> (AsteroidState, Ball) {
    let ball = Ball {
        id: aid,
        type_id: tid,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 80_000.0,
        radius: rad,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    let st = AsteroidState {
        asteroid_id: aid,
        belt_id: 0,
        type_id: tid,
        initial_units: units,
        remaining_units: units,
        unit_volume: uvol,
        depleted: false,
        respawn_at_tick: None,
    };
    (st, ball)
}

pub(crate) fn tick_sov_prospecting_deposit_respawn_core(
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
    sys_id: u32,
    now: u64,
    live_ast_states: Vec<(i64, i64)>,
) -> Vec<(i64, TypeId, u64, f64, f64, DVec3)> {
    let mut modelled: HashMap<TypeId, (u64, ResolvedOreSite)> = HashMap::new();
    for def in SOV_PROSPECTING_DEPOSITS {
        if let Ok(site) = resolve_ore_site(sde, &def.site) {
            if site
                .asteroids
                .iter()
                .all(|a| sde.types.contains_key(&a.type_id))
            {
                modelled.insert(def.upgrade_type_id, (def.respawn_seconds, site));
            }
        }
    }
    let sov_pos = derive_system_site_position(sde, sys_id, 3, 5.0);

    let needs_mut = db.with_snapshot(|snap| {
        if live_ast_states.iter().any(|&(aid, rem)| {
            snap.asteroids.get(&aid).is_some_and(|a| a.quantity != rem)
        }) {
            return true;
        }
        let installed_upgrade = snap
            .structures
            .values()
            .filter(|s| s.solar_system_id == sys_id)
            .find_map(|s| {
                snap.items.values().find_map(|it| {
                    if it.location_id == s.structure_id
                        && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                    {
                        modelled.get(&it.type_id)
                    } else {
                        None
                    }
                })
            });
        let anoms = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let needs_clear = anoms.iter().any(|anom| {
            if anom.anomaly_type != "SovOre" {
                return false;
            }
            let anom_pos = DVec3::from_array(anom.position);
            let any_remaining = snap.asteroids.values().any(|a| {
                a.solar_system_id == sys_id
                    && a.quantity > 0
                    && DVec3::new(a.position.0, a.position.1, a.position.2).distance(anom_pos)
                        <= 50_000.0
            });
            !any_remaining && (!anom.cleared || anom.lifetime_seconds == 0)
        });
        if needs_clear {
            return true;
        }
        let Some((respawn_secs, _)) = installed_upgrade else {
            return false;
        };
        let has_uncleared = anoms.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared);
        !has_uncleared
            && anoms.iter().any(|a| {
                a.anomaly_type == "SovOre"
                    && a.cleared
                    && now >= a.created_unix.saturating_add(a.lifetime_seconds.max(*respawn_secs))
            })
    });
    if !needs_mut {
        return Vec::new();
    }

    db.with_mut_snapshot(|snap| {
        for &(aid, rem) in &live_ast_states {
            if let Some(ast) = snap.asteroids.get_mut(&aid) {
                ast.quantity = rem;
            }
        }

        let installed_upgrade = snap
            .structures
            .values()
            .filter(|s| s.solar_system_id == sys_id)
            .find_map(|s| {
                snap.items.values().find_map(|it| {
                    if it.location_id == s.structure_id
                        && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                    {
                        modelled.get(&it.type_id)
                    } else {
                        None
                    }
                })
            });
        let default_respawn_secs = installed_upgrade.map(|(r, _)| *r).unwrap_or(36_000);

        let mut newly_cleared_anom_ids = Vec::new();
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "SovOre") {
                let anom_pos = DVec3::from_array(anom.position);
                let any_remaining = snap.asteroids.values().any(|a| {
                    a.solar_system_id == sys_id
                        && a.quantity > 0
                        && DVec3::new(a.position.0, a.position.1, a.position.2).distance(anom_pos)
                            <= 50_000.0
                });
                if !any_remaining {
                    if !anom.cleared {
                        anom.cleared = true;
                        anom.remaining_volume = 0.0;
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
                        newly_cleared_anom_ids.push(anom.anomaly_id.clone());
                    } else if anom.lifetime_seconds == 0 {
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
                    }
                }
            }
        }
        if !newly_cleared_anom_ids.is_empty() {
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                for s in sigs.iter_mut() {
                    if newly_cleared_anom_ids.contains(&s.signature_id) {
                        s.collapsed = true;
                        s.signal_strength = 0.0;
                    }
                }
            }
        }

        let Some((respawn_secs, site)) = installed_upgrade else {
            return Vec::new();
        };
        let has_uncleared = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared));
        let ready_to_respawn = !has_uncleared
            && snap.cosmic_anomalies.get(&sys_id).is_some_and(|list| {
                list.iter().any(|a| {
                    a.anomaly_type == "SovOre"
                        && a.cleared
                        && now >= a.created_unix.saturating_add(a.lifetime_seconds.max(*respawn_secs))
                })
            });
        if !ready_to_respawn {
            return Vec::new();
        }

        let sov_pos_vec = DVec3::from_array(sov_pos);
        snap.asteroids.retain(|_, a| {
            !(a.solar_system_id == sys_id
                && a.belt_id == 0
                && DVec3::new(a.position.0, a.position.1, a.position.2).distance(sov_pos_vec)
                    <= 50_000.0)
        });
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            anoms.retain(|a| a.anomaly_type != "SovOre");
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            sigs.retain(|s| s.sig_group != "SovOre");
        }

        let anom_seq = snap.alloc_entity_id();
        let anom_id = format!("DNG-{}-{}", site.dungeon_id, anom_seq);
        let mut volume = 0.0_f64;
        let mut spawned = Vec::with_capacity(site.asteroids.len());
        for (i, a) in site.asteroids.iter().enumerate() {
            let off = asteroid_offset(i);
            let pos = [sov_pos[0] + off[0], sov_pos[1] + off[1], sov_pos[2] + off[2]];
            let (uvol, rad) = type_vol_rad_from_sde(sde, a.type_id);
            volume += a.units as f64 * uvol;
            let aid = snap.alloc_entity_id();
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: a.type_id,
                    quantity: a.units,
                    initial_quantity: a.units,
                    position: (pos[0], pos[1], pos[2]),
                    radius: rad,
                },
            );
            spawned.push((aid, a.type_id, a.units as u64, uvol, rad, DVec3::from_array(pos)));
        }
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: anom_id.clone(),
            solar_system_id: sys_id,
            name: site.name.clone(),
            anomaly_type: "SovOre".into(),
            position: sov_pos,
            signal_strength: 1.0,
            ore_types: site.ore_types(),
            remaining_volume: volume,
            created_unix: now,
            lifetime_seconds: 0,
            cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: anom_id,
            solar_system_id: sys_id,
            sig_group: "SovOre".into(),
            type_name: site.name.clone(),
            position: sov_pos,
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: now,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
        spawned
    })
}

/// Evaluates server-clock respawn timers for depleted Sovereignty Hub prospecting deposits in `sys_id`.
pub async fn tick_sov_prospecting_deposit_respawn(ctx: &ServerContext, sys_id: u32) {
    let Some(sov_asteroid_ids) = collect_sov_asteroid_ids(&ctx.db, sys_id) else {
        return;
    };

    let sys_handle_opt = ctx.systems.get_handle(sys_id);
    let mut live_ast_states = Vec::with_capacity(sov_asteroid_ids.len());
    if let Some(ref sys_handle) = sys_handle_opt {
        for aid in sov_asteroid_ids {
            if let Ok(Some(st)) = sys_handle.get_asteroid_state(aid).await {
                live_ast_states.push((
                    aid,
                    if st.depleted {
                        0_i64
                    } else {
                        st.remaining_units as i64
                    },
                ));
            }
        }
    }

    let newly_spawned = tick_sov_prospecting_deposit_respawn_core(
        &ctx.sde,
        &ctx.db,
        sys_id,
        ctx.now_unix(),
        live_ast_states,
    );
    if let Some(sys_handle) = sys_handle_opt {
        for (aid, tid, units, uvol, rad, pos) in newly_spawned {
            let (st, ball) = build_sov_asteroid_ball_and_state(aid, tid, units, uvol, rad, pos);
            let _ = sys_handle.spawn_asteroid(st, ball).await;
        }
    }
}

pub(crate) fn tick_sov_prospecting_deposit_respawn_with_handle(
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
    handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    now: u64,
) {
    let Some(sov_asteroid_ids) = collect_sov_asteroid_ids(db, sys_id) else {
        return;
    };
    let live_ast_states = handle.live_asteroid_remaining_units(&sov_asteroid_ids);
    let newly_spawned =
        tick_sov_prospecting_deposit_respawn_core(sde, db, sys_id, now, live_ast_states);
    for (aid, tid, units, uvol, rad, pos) in newly_spawned {
        let (st, ball) = build_sov_asteroid_ball_and_state(aid, tid, units, uvol, rad, pos);
        handle.try_spawn_asteroid(st, ball);
    }
}

pub(crate) fn tick_sov_prospecting_deposit_respawn_in_seed_ctx(
    seed_ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
) {
    if !seed_ctx.db_read_safe {
        return;
    }
    let sys_id = seed_ctx.solar_system_id;
    let Some(sov_asteroid_ids) = collect_sov_asteroid_ids(db, sys_id) else {
        return;
    };
    let live_ast_states: Vec<(i64, i64)> = sov_asteroid_ids
        .into_iter()
        .filter_map(|aid| {
            seed_ctx.actor_state.asteroids.get(&aid).map(|st| {
                (
                    aid,
                    if st.depleted {
                        0_i64
                    } else {
                        st.remaining_units as i64
                    },
                )
            })
        })
        .collect();
    let newly_spawned = tick_sov_prospecting_deposit_respawn_core(
        sde,
        db,
        sys_id,
        seed_ctx.now_unix_secs(),
        live_ast_states,
    );
    for (aid, tid, units, uvol, rad, pos) in newly_spawned {
        let (st, ball) = build_sov_asteroid_ball_and_state(aid, tid, units, uvol, rad, pos);
        seed_ctx.spawn_asteroid(st, ball);
    }
}

struct SeededMiningPlanDbResult {
    is_pochven: bool,
    newly_spawned: Vec<(i64, TypeId, u64, f64, f64, DVec3)>,
    guard_waves: Vec<(i64, TypeId, DVec3)>,
    pochven_ore_sites: Vec<(u32, [f64; 3])>,
    was_already_seeded: bool,
}

fn plan_and_seed_mining_sites_in_db(
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
    mechanic_rng: &crate::rng::MechanicRng,
    sys_id: u32,
    now: u64,
) -> Option<SeededMiningPlanDbResult> {
    let sys = sde.solar_systems.get(&sys_id)?;
    let region = region_name(sde, sys_id).map(str::to_owned);

    enum Planned {
        Ore(SiteKind, ResolvedOreSite),
        Gas(ResolvedGasSite),
        WhOre(u32, &'static WormholeOreSiteDef, Vec<(TypeId, i64)>),
        WhGas(u32, &'static str, Vec<(TypeId, i64)>),
        PochvenOre(u32, String, Vec<(TypeId, i64)>),
    }
    let mut planned: Vec<Planned> = Vec::new();
    for def in COSMIC_ORE_SITES.iter().filter(|d| d.band.matches_system(sys)) {
        match resolve_ore_site(sde, def) {
            Ok(site) if site.asteroids.iter().all(|a| sde.types.contains_key(&a.type_id)) => {
                planned.push(Planned::Ore(def.kind, site))
            }
            Ok(site) => warn!(
                site = site.name,
                "ore site references an asteroid type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "ore site not spawned"),
        }
    }
    if let Some(def) = GAS_SITES
        .iter()
        .find(|d| d.band.matches_system(sys) && region.as_deref().is_some_and(|r| d.region_names.contains(&r)))
    {
        match resolve_gas_site(sde, def) {
            Ok(site) if sde.types.contains_key(&site.gas_type_id) => planned.push(Planned::Gas(site)),
            Ok(site) => warn!(
                site = site.name,
                "gas site references a gas type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "gas site not spawned"),
        }
    }
    let is_pochven = sys.region_id == 10_000_070;
    if is_pochven {
        const POCHVEN_SPECIAL_ORE_DUNGEONS: [u32; 5] = [8987, 8988, 8989, 8990, 8991];
        let primary_did = POCHVEN_SPECIAL_ORE_DUNGEONS[(sys_id as usize) % POCHVEN_SPECIAL_ORE_DUNGEONS.len()];
        for did in [primary_did, 8974] {
            if let (Some(dng), Some(layout)) = (
                sde.dungeons.get(&did),
                sde.dungeon_asteroids.get(&did).filter(|v| !v.is_empty()),
            ) {
                let mut rocks = Vec::new();
                let mut valid = true;
                for entry in layout {
                    if !sde.types.contains_key(&entry.type_id) {
                        valid = false;
                        break;
                    }
                    let rock_count = entry.asteroid_count.clamp(1, 6);
                    for _ in 0..rock_count {
                        rocks.push((entry.type_id, 4_000_i64));
                    }
                }
                if valid && !rocks.is_empty() {
                    planned.push(Planned::PochvenOre(did, dng.name.trim().to_string(), rocks));
                }
            }
        }
    }
    let wh_class_opt = sys
        .wormhole_class_id
        .or_else(|| sde.constellations.get(&sys.constellation_id).and_then(|c| c.wormhole_class_id))
        .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.wormhole_class_id))
        .map(|c| c as u8);
    if (31_000_000..32_000_000).contains(&sys_id) || wh_class_opt.is_some_and(|c| matches!(c, 1..=6 | 12..=18)) {
        for ore_def in sample_wormhole_ore_sites_for_system(
            sde,
            mechanic_rng,
            sys_id,
            wh_class_opt.unwrap_or(3),
        ) {
            if let Ok((did, rocks)) = resolve_wormhole_ore_site_with_dungeon(sde, ore_def) {
                planned.push(Planned::WhOre(did, ore_def, rocks));
            }
        }
        if let Some(wh_def) = WORMHOLE_GAS_SITES.first() {
            if let Ok((did, clouds)) = resolve_wormhole_gas_site(sde, wh_def) {
                planned.push(Planned::WhGas(did, wh_def.dungeon_name, clouds));
            }
        }
    }
    if planned.is_empty() {
        return None;
    }

    let mut wh_ore_idx: usize = 0;
    let mut pochven_ore_idx: usize = 0;
    let centres: Vec<[f64; 3]> = planned
        .iter()
        .map(|p| match p {
            Planned::Ore(SiteKind::Anomaly, _) => derive_system_site_position(sde, sys_id, 0, 4.0),
            Planned::Ore(SiteKind::Signature, _) => derive_system_site_position(sde, sys_id, 1, 6.5),
            Planned::Gas(_) | Planned::WhGas(_, _, _) => {
                derive_system_site_position(sde, sys_id, 2, 8.5)
            }
            Planned::WhOre(_, _, _) => {
                let idx = wh_ore_idx;
                wh_ore_idx += 1;
                derive_system_site_position(sde, sys_id, 3 + idx, 5.5 + (idx as f64) * 1.1)
            }
            Planned::PochvenOre(_, _, _) => {
                let idx = pochven_ore_idx;
                pochven_ore_idx += 1;
                derive_system_site_position(sde, sys_id, 10 + idx, 4.5 + (idx as f64) * 1.3)
            }
        })
        .collect();

    let (newly_spawned, guard_waves, pochven_ore_sites, was_already_seeded) =
        db.with_mut_snapshot(|snap| {
            let already_seeded = snap
                .cosmic_anomalies
                .get(&sys_id)
                .is_some_and(|l| l.iter().any(|a| a.anomaly_type == "OreAnomaly"))
                || snap
                    .cosmic_signatures
                    .get(&sys_id)
                    .is_some_and(|l| l.iter().any(|s| matches!(s.sig_group.as_str(), "OreAnomaly" | "Ore" | "Gas")));
            if already_seeded {
                return (Vec::new(), Vec::new(), Vec::new(), true);
            }

            let mut spawned = Vec::new();
            let mut guards: Vec<(i64, TypeId, DVec3)> = Vec::new();
            let mut pochven_sites: Vec<(u32, [f64; 3])> = Vec::new();
            for (p, centre) in planned.iter().zip(centres.iter().copied()) {
                let seq = snap.alloc_entity_id();
                let blank_sig = |id: String, group: &str, name: String, strength: f64| CosmicSignatureRecord {
                    signature_id: id,
                    solar_system_id: sys_id,
                    sig_group: group.into(),
                    type_name: name,
                    signal_strength: strength,
                    position: centre,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                };
                let rocks: Vec<(TypeId, i64)> = match p {
                    Planned::Ore(kind, site) => {
                        let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                        match kind {
                            SiteKind::Anomaly => {
                                let volume: f64 = site
                                    .asteroids
                                    .iter()
                                    .map(|a| a.units as f64 * type_vol_rad_from_sde(sde, a.type_id).0)
                                    .sum();
                                snap.cosmic_anomalies
                                    .entry(sys_id)
                                    .or_default()
                                    .push(CosmicAnomalyRecord {
                                        anomaly_id: site_id.clone(),
                                        solar_system_id: sys_id,
                                        name: site.name.clone(),
                                        anomaly_type: "OreAnomaly".into(),
                                        position: centre,
                                        signal_strength: 1.0,
                                        ore_types: site.ore_types(),
                                        remaining_volume: volume,
                                        created_unix: now,
                                        lifetime_seconds: 86_400,
                                        cleared: false,
                                    });
                                snap.cosmic_signatures
                                    .entry(sys_id)
                                    .or_default()
                                    .push(blank_sig(site_id, "OreAnomaly", site.name.clone(), 1.0));
                            }
                            SiteKind::Signature => {
                                snap.cosmic_signatures
                                    .entry(sys_id)
                                    .or_default()
                                    .push(blank_sig(site_id, "Ore", site.name.clone(), 0.0));
                            }
                        }
                        site.asteroids.iter().map(|a| (a.type_id, a.units)).collect()
                    }
                    Planned::Gas(site) => {
                        let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                        snap.cosmic_signatures
                            .entry(sys_id)
                            .or_default()
                            .push(blank_sig(site_id, "Gas", site.name.clone(), 0.0));
                        site.cloud_units.iter().map(|&u| (site.gas_type_id, u)).collect()
                    }
                    Planned::WhOre(did, ore_def, rocks) => {
                        let site_id = format!("DNG-{did}-{seq}");
                        let volume: f64 = rocks
                            .iter()
                            .map(|&(tid, u)| (u as f64) * type_vol_rad_from_sde(sde, tid).0)
                            .sum();
                        let mut ore_types: Vec<TypeId> = rocks.iter().map(|&(tid, _)| tid).collect();
                        ore_types.sort_unstable();
                        ore_types.dedup();
                        snap.cosmic_anomalies
                            .entry(sys_id)
                            .or_default()
                            .push(CosmicAnomalyRecord {
                                anomaly_id: site_id.clone(),
                                solar_system_id: sys_id,
                                name: ore_def.dungeon_name.to_string(),
                                anomaly_type: "OreAnomaly".into(),
                                position: centre,
                                signal_strength: 1.0,
                                ore_types,
                                remaining_volume: volume,
                                created_unix: now,
                                lifetime_seconds: 86_400,
                                cleared: false,
                            });
                        record_initial_wormhole_guard_wave(
                            snap,
                            &site_id,
                            centre,
                            ore_def.guards,
                            &mut guards,
                        );
                        snap.cosmic_signatures
                            .entry(sys_id)
                            .or_default()
                            .push(blank_sig(
                                site_id,
                                "OreAnomaly",
                                ore_def.dungeon_name.to_string(),
                                1.0,
                            ));
                        rocks.clone()
                    }
                    Planned::WhGas(did, name, clouds) => {
                        let site_id = format!("DNG-{did}-{seq}");
                        snap.cosmic_signatures
                            .entry(sys_id)
                            .or_default()
                            .push(blank_sig(site_id, "Gas", (*name).to_string(), 0.0));
                        clouds.clone()
                    }
                    Planned::PochvenOre(did, name, rocks) => {
                        let site_id = format!("DNG-{did}-{seq}");
                        let volume: f64 = rocks
                            .iter()
                            .map(|&(tid, u)| (u as f64) * type_vol_rad_from_sde(sde, tid).0)
                            .sum();
                        let mut ore_types: Vec<TypeId> = rocks.iter().map(|&(tid, _)| tid).collect();
                        ore_types.sort_unstable();
                        ore_types.dedup();
                        snap.cosmic_anomalies
                            .entry(sys_id)
                            .or_default()
                            .push(CosmicAnomalyRecord {
                                anomaly_id: site_id.clone(),
                                solar_system_id: sys_id,
                                name: name.clone(),
                                anomaly_type: "OreAnomaly".into(),
                                position: centre,
                                signal_strength: 1.0,
                                ore_types,
                                remaining_volume: volume,
                                created_unix: now,
                                lifetime_seconds: 86_400,
                                cleared: false,
                            });
                        snap.cosmic_signatures
                            .entry(sys_id)
                            .or_default()
                            .push(blank_sig(site_id, "OreAnomaly", name.clone(), 1.0));
                        pochven_sites.push((*did, centre));
                        rocks.clone()
                    }
                };

                for (i, (tid, units)) in rocks.into_iter().enumerate() {
                    let off = asteroid_offset(i);
                    let pos = [centre[0] + off[0], centre[1] + off[1], centre[2] + off[2]];
                    let (uvol, rad) = type_vol_rad_from_sde(sde, tid);
                    let aid = snap.alloc_entity_id();
                    snap.asteroids.insert(
                        aid,
                        AsteroidRecord {
                            asteroid_id: aid,
                            solar_system_id: sys_id,
                            belt_id: 0,
                            type_id: tid,
                            quantity: units,
                            initial_quantity: units,
                            position: (pos[0], pos[1], pos[2]),
                            radius: rad,
                        },
                    );
                    spawned.push((aid, tid, units as u64, uvol, rad, DVec3::from_array(pos)));
                }
            }
            (spawned, guards, pochven_sites, false)
        });

    Some(SeededMiningPlanDbResult {
        is_pochven,
        newly_spawned,
        guard_waves,
        pochven_ore_sites,
        was_already_seeded,
    })
}

/// Seeds and rehydrates cosmic ore/gas/WH/Pochven/SovOre/IceBelt/Phased Asteroid Field resources
/// inside `Cold -> Loading -> Live` activation (`#1542`).
pub(crate) fn seed_resource_sites_in_activation(
    seed_ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
    _mechanic_rng: &crate::rng::MechanicRng,
    phased_fields: &dyn eve_db::lock_rank::AnyRwLock<HashMap<u32, PhasedAsteroidFieldState>>,
) {
    if !seed_ctx.db_read_safe {
        return;
    }
    let sys_id = seed_ctx.solar_system_id;

    tick_sov_prospecting_deposit_respawn_in_seed_ctx(seed_ctx, sde, db);
    super::space_mining_harvest::tick_ice_belt_respawn_in_seed_ctx(seed_ctx, sde, db);
    super::mining_phased_autotarget::rehydrate_phased_asteroid_field_in_seed_ctx(
        seed_ctx,
        sde,
        db,
        phased_fields,
    );

    let persisted_non_belt_asteroids: Vec<AsteroidRecord> = db.with_snapshot(|snap| {
        snap.asteroids
            .values()
            .filter(|a| a.solar_system_id == sys_id && a.belt_id == 0 && a.quantity > 0)
            .cloned()
            .collect()
    });
    for rec in persisted_non_belt_asteroids {
        if !seed_ctx.ballpark.balls.contains_key(&rec.asteroid_id) {
            let Some(t) = sde.types.get(&rec.type_id) else {
                continue;
            };
            let (uvol, default_rad) = (t.volume.max(0.001), t.radius.max(50.0));
            let rad = if rec.radius > 0.0 {
                rec.radius
            } else {
                default_rad
            };
            let (mut st, ball) =
                super::space_mining_harvest::build_ice_belt_asteroid_ball_and_state(
                    rec.asteroid_id,
                    rec.type_id,
                    rec.quantity.max(0) as u64,
                    uvol,
                    rad,
                    DVec3::new(rec.position.0, rec.position.1, rec.position.2),
                    50_000.0,
                );
            st.initial_units = rec.initial_quantity.max(rec.quantity).max(0) as u64;
            seed_ctx.spawn_asteroid(st, ball);
        }
    }

    super::space_mining_harvest::hydrate_wormhole_ore_guards_in_seed_ctx(seed_ctx, sde, db);

    let is_pochven = sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    if is_pochven {
        // Prune stale Hypnosian AI entries from a prior evicted actor generation (`F-06`).
        let has_stale_ai = {
            let rt = combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
            rt.npc_entities.iter().any(|(bid, n)| {
                n.solar_system_id == sys_id
                    && n.archetype == "PochvenHypnosianSleeper"
                    && !seed_ctx.ballpark.balls.contains_key(bid)
            })
        };
        if has_stale_ai {
            let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            rt.npc_entities.retain(|bid, n| {
                !(n.solar_system_id == sys_id
                    && n.archetype == "PochvenHypnosianSleeper"
                    && !seed_ctx.ballpark.balls.contains_key(bid))
            });
        }

        let existing_positions: Vec<DVec3> = {
            let rt = combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
            rt.npc_entities
                .values()
                .filter(|n| {
                    n.solar_system_id == sys_id && n.archetype == "PochvenHypnosianSleeper"
                })
                .filter_map(|n| seed_ctx.ballpark.balls.get(&n.ball_id).map(|b| b.position))
                .collect()
        };
        let pochven_sites: Vec<(u32, [f64; 3])> = db.with_snapshot(|snap| {
            snap.cosmic_anomalies
                .get(&sys_id)
                .map(|list| {
                    list.iter()
                        .filter(|a| a.anomaly_type == "OreAnomaly" && !a.cleared)
                        .filter_map(|a| {
                            let did = a
                                .anomaly_id
                                .strip_prefix("DNG-")?
                                .split('-')
                                .next()?
                                .parse::<u32>()
                                .ok()?;
                            let c = DVec3::from_array(a.position);
                            let guarded = existing_positions.iter().any(|p| p.distance(c) <= 30_000.0);
                            (!guarded).then_some((did, a.position))
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        if !pochven_sites.is_empty() {
            let specs = collect_pochven_hypnosian_defender_specs(&pochven_sites);
            if !specs.is_empty() {
                super::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch_in_seed_ctx(
                    seed_ctx,
                    sde,
                    db,
                    combat_ephemeral,
                    &specs,
                );
            }
        }
    }
}

/// Periodic per-system resource maintenance (`SovOre` + `IceBelt` respawns) on an already-live
/// system (`#1542`).
pub(crate) fn tick_resource_sites_for_system_sync(
    sde: &SdeRegistry,
    db: &eve_db::WorldRepository,
    handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    now: u64,
) {
    tick_sov_prospecting_deposit_respawn_with_handle(sde, db, handle, sys_id, now);
    super::space_mining_harvest::tick_ice_belt_respawn_with_handle(sde, db, handle, sys_id, now);
}

/// Seeds the cosmic ore anomaly, hidden ore signature and gas site for `sys_id`, once per system.
pub async fn seed_system_mining_sites_from_sde(ctx: &ServerContext, sys_id: u32) {
    tick_sov_prospecting_deposit_respawn(ctx, sys_id).await;
    super::space_mining_harvest::tick_ice_belt_respawn_sync(ctx, sys_id);
    let Some(res) = plan_and_seed_mining_sites_in_db(
        &ctx.sde,
        &ctx.db,
        &ctx.mechanic_rng,
        sys_id,
        ctx.now_unix(),
    ) else {
        return;
    };

    if res.was_already_seeded {
        hydrate_wormhole_ore_guards_if_needed(ctx, sys_id).await;
        if res.is_pochven {
            spawn_pochven_ore_hypnosian_defenders_if_missing(ctx, sys_id).await;
        }
        return;
    }
    spawn_seeded_site_entities(ctx, sys_id, res.newly_spawned, res.guard_waves).await;
    if !res.pochven_ore_sites.is_empty() {
        spawn_pochven_ore_hypnosian_defenders(ctx, sys_id, &res.pochven_ore_sites).await;
    }
    super::mining_phased_autotarget::maybe_spawn_phased_asteroid_field(ctx, sys_id).await;
}

fn collect_pochven_hypnosian_defender_specs(
    sites: &[(u32, [f64; 3])],
) -> Vec<(TypeId, &'static str, u32, DVec3)> {
    let mut specs: Vec<(TypeId, &'static str, u32, DVec3)> = Vec::new();
    for &(did, centre) in sites {
        let base = DVec3::from_array(centre);
        for (i, &tid) in [56_337_u32, 56_328, 56_319].iter().enumerate() {
            let offset = DVec3::new((i as f64 - 1.0) * 3_500.0, 1_500.0 * (i as f64), 0.0);
            specs.push((tid, "PochvenHypnosianSleeper", did, base + offset));
        }
    }
    specs
}

async fn spawn_pochven_ore_hypnosian_defenders(
    ctx: &ServerContext,
    sys_id: u32,
    sites: &[(u32, [f64; 3])],
) {
    if ctx.systems.get_handle(sys_id).is_none() {
        return;
    }
    let specs = collect_pochven_hypnosian_defender_specs(sites);
    if !specs.is_empty() {
        super::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch(
            ctx,
            sys_id,
            &specs,
            |_| {},
        )
        .await;
    }
}

async fn spawn_pochven_ore_hypnosian_defenders_if_missing(ctx: &ServerContext, sys_id: u32) {
    let Some(sys_handle) = ctx.systems.get_handle(sys_id) else {
        return;
    };
    let existing_positions: Vec<DVec3> =
        super::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| n.solar_system_id == sys_id && n.archetype == "PochvenHypnosianSleeper")
                .filter_map(|n| sys_handle.get_ball_position_sync(n.ball_id))
                .collect()
        });
    let ore_centres: Vec<(u32, [f64; 3])> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&sys_id)
            .map(|list| {
                list.iter()
                    .filter(|a| a.anomaly_type == "OreAnomaly" && !a.cleared)
                    .filter_map(|a| {
                        let did = a.anomaly_id.strip_prefix("DNG-")?.split('-').next()?.parse::<u32>().ok()?;
                        let c = DVec3::from_array(a.position);
                        let guarded = existing_positions.iter().any(|p| p.distance(c) <= 30_000.0);
                        (!guarded).then_some((did, a.position))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    if !ore_centres.is_empty() {
        spawn_pochven_ore_hypnosian_defenders(ctx, sys_id, &ore_centres).await;
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("sovHubSvc", "SaveUpgradeConfiguration") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if sys_sec > 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "NotNullSecSystem".into(),
                    message: format!(
                        "Sovereignty Hub prospecting upgrades can only be installed in null-sec systems (system {sys_id} has sec {sys_sec:.2})"
                    ),
                    client_error: None,
                }));
            }

            let is_sov_upgrade_type = |tid: u32| -> bool {
                ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 4768)
            };
            let type_from_rep = |v: &PyRep| -> Option<u32> {
                v.dict_get("typeID")
                    .or_else(|| v.dict_get("upgradeTypeID"))
                    .and_then(|x| x.as_i64())
                    .or_else(|| match v {
                        PyRep::Tuple(t) => t.first().and_then(|x| x.as_i64()),
                        _ => v.as_i64(),
                    })
                    .and_then(|x| u32::try_from(x).ok())
                    .filter(|&tid| is_sov_upgrade_type(tid))
            };
            let requested_upgrade_type: Option<u32> = args.iter().find_map(|a| match a {
                PyRep::List(items) | PyRep::Tuple(items) => items.iter().find_map(type_from_rep),
                other => type_from_rep(other),
            });

            let mut modelled: HashMap<u32, (u8, u64, ResolvedOreSite)> = HashMap::new();
            for def in SOV_PROSPECTING_DEPOSITS {
                match resolve_ore_site(&ctx.sde, &def.site) {
                    Ok(site) => {
                        modelled.insert(def.upgrade_type_id, (def.level, def.respawn_seconds, site));
                    }
                    Err(e) => warn!(
                        upgrade_type_id = def.upgrade_type_id,
                        error = e,
                        "prospecting deposit not resolvable"
                    ),
                }
            }

            let sov_pos = derive_system_site_position_from_sde(ctx, sys_id, 3, 5.0);
            let type_vol_rad = |tid: u32| -> (f64, f64) {
                ctx.sde
                    .types
                    .get(&tid)
                    .map(|t| (t.volume.max(0.001), t.radius.max(60.0)))
                    .unwrap_or((0.001, 60.0))
            };
            let now = ctx.now_unix();
            let active_ship_id = session.ship_id;
            let sys_handle_opt = ctx.systems.get_handle(sys_id);
            let active_ship_pos: Option<DVec3> = if let Some(sid) = active_ship_id {
                if let Some(ref sys_handle) = sys_handle_opt {
                    sys_handle
                        .get_ball_snapshot(sid)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| b.position)
                        .or_else(|| {
                            ctx.db
                                .with_snapshot(|s| s.item_positions.get(&sid).copied())
                                .map(DVec3::from_array)
                        })
                } else {
                    ctx.db
                        .with_snapshot(|s| s.item_positions.get(&sid).copied())
                        .map(DVec3::from_array)
                }
            } else {
                None
            };
            let sys_ast_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.asteroids
                    .values()
                    .filter(|a| a.solar_system_id == sys_id && a.belt_id == 0)
                    .map(|a| a.asteroid_id)
                    .collect()
            });
            let mut live_ast_states = Vec::with_capacity(sys_ast_ids.len());
            if let Some(ref sys_handle) = sys_handle_opt {
                for aid in sys_ast_ids {
                    if let Ok(Some(st)) = sys_handle.get_asteroid_state(aid).await {
                        live_ast_states.push((aid, st.remaining_units as i64));
                    }
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id))
                    .unwrap_or((0, None));
                let held_alliance = snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|st| st.sov_alliance_id);
                let Some(caller_aid) = caller_alliance_id else {
                    return Err(BridgeRpcError {
                        code: "SovereigntyNotClaimed".into(),
                        message: format!(
                            "Caller must belong to the sovereignty-holding alliance in system {sys_id}"
                        ),
                        client_error: None,
                    });
                };
                if held_alliance != Some(caller_aid) {
                    return Err(BridgeRpcError {
                        code: "SovereigntyNotClaimed".into(),
                        message: format!(
                            "System {sys_id} sovereignty is not held by caller's alliance {caller_aid}"
                        ),
                        client_error: None,
                    });
                }

                let (is_ceo, member_role) = snap.effective_corp_roles_at_db(
                    caller_corp_id,
                    char_id,
                    None,
                    ctx.now_unix(),
                );
                if !is_ceo && (member_role & (1 | 128 | 1024 | eve_db::corp_roles::ROLE_STATION_MANAGER)) == 0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientCorporationRole".into(),
                        message: "Configuring Sovereignty Hub upgrades requires Director or Station Manager role".into(),
                        client_error: None,
                    });
                }

                let hub_struct = snap
                    .structures
                    .values()
                    .filter(|s| {
                        s.solar_system_id == sys_id
                            && s.owner_corp_id == caller_corp_id
                            && s.state == "ShieldVulnerable"
                            && s.quantum_core_installed
                            && s.fuel_blocks > 0
                    })
                    .map(|s| s.structure_id)
                    .min();
                let Some(hub_id) = hub_struct else {
                    return Err(BridgeRpcError {
                        code: "NoFriendlySovStructureInSystem".into(),
                        message: format!(
                            "Configuring Sovereignty Hub upgrades in system {sys_id} requires an online structure owned by corporation {caller_corp_id}"
                        ),
                        client_error: None,
                    });
                };
                let hub_pos = snap
                    .structures
                    .get(&hub_id)
                    .map(|s| DVec3::from_array(s.position));

                let mut found_locked_local = false;
                let mut found_out_of_range_ship = false;
                let mut upgrade_item_id: Option<i64> = None;
                for it in snap.items.values() {
                    if it.quantity <= 0 || !is_sov_upgrade_type(it.type_id) {
                        continue;
                    }
                    if !requested_upgrade_type.map_or(true, |rt| it.type_id == rt) {
                        continue;
                    }
                    let already_installed_here =
                        it.location_id == hub_id && it.flag == eve_db::InventoryFlag::SubSystemSlot0;
                    let personal_ok = it.owner_id == char_id
                        && matches!(
                            it.flag,
                            eve_db::InventoryFlag::Hangar
                                | eve_db::InventoryFlag::Cargo
                                | eve_db::InventoryFlag::SubSystemSlot0
                                | eve_db::InventoryFlag::Locked
                        );
                    let corp_ok = caller_corp_id > 0
                        && it.owner_id == caller_corp_id
                        && (already_installed_here
                            || it.flag == eve_db::InventoryFlag::Locked
                            || (matches!(
                                it.flag,
                                eve_db::InventoryFlag::Hangar
                                    | eve_db::InventoryFlag::CorpSAG1
                                    | eve_db::InventoryFlag::CorpSAG2
                                    | eve_db::InventoryFlag::CorpSAG3
                                    | eve_db::InventoryFlag::CorpSAG4
                                    | eve_db::InventoryFlag::CorpSAG5
                                    | eve_db::InventoryFlag::CorpSAG6
                                    | eve_db::InventoryFlag::CorpSAG7
                            ) && crate::services::mining_advanced::mining_compression::caller_can_take_corp_hangar_item(
                                snap,
                                char_id,
                                caller_corp_id,
                                it.flag,
                            )));
                    let in_hub = it.location_id == hub_id;
                    let in_ship = active_ship_id == Some(it.location_id);
                    if (!personal_ok && !corp_ok) || (!in_hub && !in_ship) {
                        continue;
                    }
                    if in_ship && !in_hub {
                        if let (Some(s_pos), Some(h_pos)) = (active_ship_pos, hub_pos) {
                            if s_pos.length_squared() > 0.0
                                && h_pos.length_squared() > 0.0
                                && s_pos.distance(h_pos) > 50_000.0
                            {
                                found_out_of_range_ship = true;
                                continue;
                            }
                        }
                    }
                    if it.flag == eve_db::InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(it.item_id)
                    {
                        found_locked_local = true;
                        continue;
                    }
                    upgrade_item_id =
                        Some(upgrade_item_id.map_or(it.item_id, |cur| cur.min(it.item_id)));
                }

                let Some(up_iid) = upgrade_item_id else {
                    if found_out_of_range_ship {
                        return Err(BridgeRpcError {
                            code: "OutsideSovStructureProximity".into(),
                            message: "Installing a Sovereignty Hub upgrade from your ship requires being within 50 km of the hub".into(),
                            client_error: None,
                        });
                    }
                    if found_locked_local {
                        return Err(BridgeRpcError {
                            code: "UpgradeItemLocked".into(),
                            message: "Sovereignty Hub upgrade item is locked in a contract or escrow".into(),
                            client_error: None,
                        });
                    }
                    return Err(BridgeRpcError {
                        code: "UpgradeItemNotFound".into(),
                        message: "SaveUpgradeConfiguration requires an unlocked Sovereignty Hub upgrade item (group 4768) in the hub structure or your active ship".into(),
                        client_error: None,
                    });
                };
                let up_tid = snap.items.get(&up_iid).map(|i| i.type_id).unwrap_or(0);
                let Some((level, respawn_secs, site)) = modelled.get(&up_tid) else {
                    return Err(BridgeRpcError {
                        code: "ProspectingDepositNotModelled".into(),
                        message: format!(
                            "Sovereignty Hub upgrade type {up_tid} has no modelled prospecting deposit"
                        ),
                        client_error: None,
                    });
                };

                let was_already_installed = snap.items.get(&up_iid).is_some_and(|it| {
                    it.location_id == hub_id && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                });
                let split_new = snap
                    .items
                    .get(&up_iid)
                    .filter(|it| !was_already_installed && it.quantity > 1)
                    .cloned();
                if let Some(src) = split_new {
                    if let Some(up_it) = snap.items.get_mut(&up_iid) {
                        up_it.quantity -= 1;
                        up_it.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_iid = snap.next_entity_id;
                    snap.items.insert(
                        new_iid,
                        eve_db::InventoryItemRecord {
                            item_id: new_iid,
                            type_id: src.type_id,
                            owner_id: src.owner_id,
                            location_id: hub_id,
                            flag: eve_db::InventoryFlag::SubSystemSlot0,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
                } else if let Some(up_it) = snap.items.get_mut(&up_iid) {
                    up_it.location_id = hub_id;
                    up_it.flag = eve_db::InventoryFlag::SubSystemSlot0;
                    up_it.singleton = true;
                    up_it.version += 1;
                }
                snap.sov_prospecting_levels.insert(sys_id, u32::from(*level));
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    st.industrial_index = st.industrial_index.max(*level);
                }

                for &(aid, rem) in &live_ast_states {
                    if let Some(ast) = snap.asteroids.get_mut(&aid) {
                        ast.quantity = rem;
                    }
                }
                let mut newly_cleared_anom_ids = Vec::new();
                if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
                    for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "SovOre") {
                        let anom_pos = DVec3::from_array(anom.position);
                        let any_remaining = snap.asteroids.values().any(|a| {
                            a.solar_system_id == sys_id
                                && a.quantity > 0
                                && DVec3::new(a.position.0, a.position.1, a.position.2)
                                    .distance(anom_pos)
                                    <= 50_000.0
                        });
                        if !any_remaining {
                            if !anom.cleared {
                                anom.cleared = true;
                                anom.remaining_volume = 0.0;
                                anom.created_unix = now;
                                anom.lifetime_seconds = *respawn_secs;
                                newly_cleared_anom_ids.push(anom.anomaly_id.clone());
                            } else if anom.lifetime_seconds == 0 {
                                anom.created_unix = now;
                                anom.lifetime_seconds = *respawn_secs;
                            }
                        }
                    }
                }
                if !newly_cleared_anom_ids.is_empty() {
                    if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                        for s in sigs.iter_mut() {
                            if newly_cleared_anom_ids.contains(&s.signature_id) {
                                s.collapsed = true;
                                s.signal_strength = 0.0;
                            }
                        }
                    }
                }

                let existing_sov_anoms: Vec<String> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|list| {
                        list.iter()
                            .filter(|a| a.anomaly_type == "SovOre" && !a.cleared)
                            .map(|a| a.anomaly_id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if was_already_installed || !existing_sov_anoms.is_empty() {
                    return Ok((hub_id, *level, i64::from(up_tid), site.clone(), existing_sov_anoms, Vec::new()));
                }

                snap.next_entity_id += 1;
                let anom_id = format!("DNG-{}-{}", site.dungeon_id, snap.next_entity_id);
                let mut volume = 0.0_f64;
                let mut newly_spawned = Vec::with_capacity(site.asteroids.len());
                for (i, a) in site.asteroids.iter().enumerate() {
                    let off = asteroid_offset(i);
                    let pos = [sov_pos[0] + off[0], sov_pos[1] + off[1], sov_pos[2] + off[2]];
                    let (uvol, rad) = type_vol_rad(a.type_id);
                    volume += a.units as f64 * uvol;
                    snap.next_entity_id += 1;
                    let aid = snap.next_entity_id;
                    snap.asteroids.insert(
                        aid,
                        AsteroidRecord {
                            asteroid_id: aid,
                            solar_system_id: sys_id,
                            belt_id: 0,
                            type_id: a.type_id,
                            quantity: a.units,
                            initial_quantity: a.units,
                            position: (pos[0], pos[1], pos[2]),
                            radius: rad,
                        },
                    );
                    newly_spawned.push((aid, a.type_id, a.units as u64, uvol, rad, DVec3::from_array(pos)));
                }
                snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
                    anomaly_id: anom_id.clone(),
                    solar_system_id: sys_id,
                    name: site.name.clone(),
                    anomaly_type: "SovOre".into(),
                    position: sov_pos,
                    signal_strength: 1.0,
                    ore_types: site.ore_types(),
                    remaining_volume: volume,
                    created_unix: now,
                    lifetime_seconds: 0,
                    cleared: false,
                });
                snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
                    signature_id: anom_id.clone(),
                    solar_system_id: sys_id,
                    sig_group: "SovOre".into(),
                    type_name: site.name.clone(),
                    position: sov_pos,
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 0,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                });

                Ok((hub_id, *level, i64::from(up_tid), site.clone(), vec![anom_id], newly_spawned))
            });

            let (hub_id, level, upgrade_type_id, site, anom_ids, newly_spawned) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if !newly_spawned.is_empty() {
                if let Some(ref sys_handle) = sys_handle_opt {
                    for (aid, tid, units, uvol, rad, pos) in newly_spawned {
                        let ball = Ball {
                            id: aid,
                            type_id: tid,
                            position: pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: 80_000.0,
                            radius: rad,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        };
                        let st = AsteroidState {
                            asteroid_id: aid,
                            belt_id: 0,
                            type_id: tid,
                            initial_units: units,
                            remaining_units: units,
                            unit_volume: uvol,
                            depleted: false,
                            respawn_at_tick: None,
                        };
                        let _ = sys_handle.spawn_asteroid(st, ball).await;
                    }
                }
            }

            Some(Ok(PyRep::keyval([
                ("hubID", PyRep::Integer(hub_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("prospectingLevel", PyRep::Integer(i64::from(level))),
                ("upgradeTypeID", PyRep::Integer(upgrade_type_id)),
                ("online", PyRep::Bool(true)),
                (
                    "spawnedAnomalies",
                    PyRep::List(anom_ids.into_iter().map(PyRep::String).collect()),
                ),
                (
                    "oreTypes",
                    PyRep::List(
                        site.ore_types()
                            .into_iter()
                            .map(|tid| PyRep::Integer(i64::from(tid)))
                            .collect(),
                    ),
                ),
            ])))
        }

        ("sovHubSvc", "GetInstalledUpgradesLocal")
        | ("sovHubSvc", "GetInstalledUpgradesForHub") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            tick_sov_prospecting_deposit_respawn(ctx, sys_id).await;
            let arg_hub_id = args.first().and_then(|a| a.as_i64());
            let (hub_id, lvl, sov_anoms, installed) = ctx.db.with_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id.or(session.alliance_id)))
                    .unwrap_or((session.corporation_id.unwrap_or(0), session.alliance_id));
                let resolved_hub = arg_hub_id.or_else(|| {
                    snap.structures
                        .values()
                        .filter(|s| {
                            let same_corp = s.owner_corp_id == caller_corp_id;
                            let same_alliance = caller_alliance_id.is_some()
                                && snap
                                    .corporations
                                    .get(&s.owner_corp_id)
                                    .and_then(|c| c.alliance_id)
                                    == caller_alliance_id;
                            s.solar_system_id == sys_id && (same_corp || same_alliance)
                        })
                        .map(|s| s.structure_id)
                        .min()
                        .or_else(|| {
                            snap.structures
                                .values()
                                .filter(|s| s.solar_system_id == sys_id)
                                .map(|s| s.structure_id)
                                .min()
                        })
                });
                let mut installed: Vec<u32> = resolved_hub
                    .map(|hid| {
                        snap.items
                            .values()
                            .filter(|it| {
                                it.location_id == hid
                                    && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 4768)
                            })
                            .map(|it| it.type_id)
                            .collect()
                    })
                    .unwrap_or_default();
                installed.sort_unstable();
                let l = snap.sov_prospecting_levels.get(&sys_id).copied().unwrap_or(0);
                let list: Vec<PyRep> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|anoms| {
                        anoms
                            .iter()
                            .filter(|a| a.anomaly_type == "SovOre" && !a.cleared)
                            .map(|a| {
                                PyRep::keyval([
                                    ("anomalyID", PyRep::String(a.anomaly_id.clone())),
                                    ("siteName", PyRep::String(a.name.clone())),
                                    ("siteType", PyRep::String(a.anomaly_type.clone())),
                                    ("signalStrength", PyRep::Real(1.0)),
                                    ("x", PyRep::Real(a.position[0])),
                                    ("y", PyRep::Real(a.position[1])),
                                    ("z", PyRep::Real(a.position[2])),
                                    (
                                        "oreTypes",
                                        PyRep::List(
                                            a.ore_types
                                                .iter()
                                                .map(|&tid| PyRep::Integer(tid as i64))
                                                .collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (resolved_hub, l, list, installed)
            });
            let upgrades: Vec<PyRep> = installed
                .into_iter()
                .map(|tid| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(i64::from(tid))),
                        ("online", PyRep::Bool(true)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::keyval([
                ("hubID", hub_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("prospectingLevel", PyRep::Integer(lvl as i64)),
                ("upgrades", PyRep::List(upgrades)),
                ("anomalies", PyRep::List(sov_anoms)),
            ])))
        }

        _ => None,
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("sovHubSvc", "SaveUpgradeConfiguration", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovHubSvc", "GetInstalledUpgradesLocal", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovHubSvc", "GetInstalledUpgradesForHub", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "mining_sites_tests.rs"]
mod mining_sites_tests;

