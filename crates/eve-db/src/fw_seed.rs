//! Faction Warfare warzone solar system registry and occupation state initialization.
//!
//! Provides the canonical list of 175 FW warzone systems and routines to backfill
//! or migrate warzone occupation state into durable snapshots across both relational SQLite
//! and JSON storage backends (`R-FW-P1-5`, `R-FW-P1-7`, `R-FW-P1-8`).

use crate::snapshot_ext::FwSystemOccupationRecord;
use crate::types::WorldStateSnapshot;

/// DERIVED external Faction Warfare warzone solar system registry (175 systems total across
/// Amarr-Minmatar and Caldari-Gallente warzones).
///
/// Provenance: Compiled from historical EVE Online Faction Warfare warzone solar systems
/// (170 standard contestable systems, 4 empire faction militia headquarters: Mehatoor, Amo,
/// Onnamon, and Intaki, and Asabona).
/// At clean server initialization, each system begins uncontested with occupier equal to its sovereign owner.
/// Format: `(solar_system_id, owner_faction_id)`.
/// Victory points threshold defaults to 75,000 for standard systems (DERIVED from wiki: Uprising expansion).
pub const DEFAULT_FW_WARZONE_SYSTEMS: &[(u32, i64)] = &[
    (30000012, 500003), (30002055, 500002), (30002056, 500002), (30002057, 500002),
    (30002058, 500002), (30002059, 500002), (30002060, 500002), (30002061, 500002),
    (30002062, 500002), (30002063, 500002), (30002064, 500002), (30002065, 500002),
    (30002066, 500002), (30002067, 500002), (30002082, 500002), (30002083, 500002),
    (30002084, 500002), (30002085, 500002), (30002086, 500002), (30002087, 500002),
    (30002088, 500002), (30002089, 500002), (30002090, 500002), (30002091, 500002),
    (30002092, 500002), (30002093, 500002), (30002094, 500002), (30002095, 500002),
    (30002096, 500002), (30002097, 500002), (30002098, 500002), (30002099, 500002),
    (30002100, 500002), (30002101, 500002), (30002102, 500002), (30002514, 500002),
    (30002516, 500002), (30002517, 500002), (30002537, 500002), (30002538, 500002),
    (30002539, 500002), (30002540, 500002), (30002541, 500002), (30002542, 500002),
    (30002756, 500001), (30002757, 500001), (30002758, 500001), (30002759, 500001),
    (30002760, 500001), (30002796, 500001), (30002806, 500001), (30002807, 500001),
    (30002808, 500001), (30002809, 500001), (30002810, 500001), (30002811, 500001),
    (30002812, 500001), (30002813, 500001), (30002957, 500003), (30002958, 500003),
    (30002959, 500003), (30002960, 500003), (30002961, 500003), (30002962, 500003),
    (30002974, 500003), (30002975, 500003), (30002976, 500003), (30002977, 500003),
    (30002978, 500003), (30002979, 500003), (30002980, 500003), (30002981, 500003),
    (30003063, 500003), (30003067, 500003), (30003068, 500003), (30003069, 500003),
    (30003070, 500003), (30003071, 500003), (30003072, 500003), (30003077, 500003),
    (30003079, 500003), (30003086, 500003), (30003087, 500003), (30003088, 500003),
    (30003089, 500003), (30003090, 500003), (30003091, 500003), (30003787, 500004),
    (30003788, 500004), (30003789, 500004), (30003790, 500004), (30003791, 500004),
    (30003792, 500004), (30003793, 500004), (30003795, 500004), (30003796, 500004),
    (30003797, 500004), (30003799, 500004), (30003825, 500004), (30003826, 500004),
    (30003827, 500004), (30003828, 500004), (30003829, 500004), (30003836, 500004),
    (30003837, 500004), (30003838, 500004), (30003839, 500004), (30003840, 500004),
    (30003841, 500004), (30003842, 500004), (30003850, 500004), (30003851, 500004),
    (30003852, 500004), (30003853, 500004), (30003854, 500004), (30003855, 500004),
    (30003856, 500004), (30003857, 500004), (30004979, 500004), (30004980, 500004),
    (30004982, 500004), (30004984, 500004), (30004985, 500004), (30004997, 500004),
    (30004999, 500004), (30005000, 500004), (30005295, 500004), (30005296, 500004),
    (30005297, 500004), (30005298, 500004), (30005299, 500004), (30005300, 500004),
    (30005320, 500004), (30005321, 500004), (30045306, 500001), (30045307, 500001),
    (30045308, 500001), (30045309, 500001), (30045310, 500001), (30045311, 500001),
    (30045312, 500001), (30045313, 500001), (30045314, 500001), (30045315, 500001),
    (30045316, 500001), (30045317, 500001), (30045318, 500001), (30045319, 500001),
    (30045320, 500001), (30045324, 500001), (30045330, 500001), (30045331, 500001),
    (30045332, 500001), (30045333, 500001), (30045334, 500001), (30045335, 500001),
    (30045336, 500001), (30045337, 500001), (30045338, 500001), (30045339, 500001),
    (30045340, 500001), (30045341, 500001), (30045342, 500001), (30045343, 500001),
    (30045344, 500001), (30045345, 500001), (30045346, 500001), (30045347, 500001),
    (30045348, 500001), (30045349, 500001), (30045350, 500001), (30045351, 500001),
    (30045352, 500001), (30045353, 500001), (30045354, 500001),
];

impl WorldStateSnapshot {
    /// Backfills default FW warzone occupations if empty, populates missing warzone systems,
    /// and migrates legacy 3,000 victory-point thresholds to 75,000 (`R-FW-P1-5`, `R-FW-P1-8`).
    pub fn backfill_fw_system_occupations(&mut self) {
        if self.fw_system_occupations.is_empty() {
            for &(sys_id, owner) in DEFAULT_FW_WARZONE_SYSTEMS {
                self.fw_system_occupations.insert(
                    sys_id,
                    FwSystemOccupationRecord {
                        solar_system_id: sys_id,
                        owner_faction_id: owner,
                        occupier_faction_id: owner,
                        victory_points: 0,
                        victory_points_threshold: 75_000,
                    },
                );
            }
        } else {
            // One-time migration: rewrite legacy 3,000 thresholds to 75,000 for standard warzone systems
            for &(sys_id, owner) in DEFAULT_FW_WARZONE_SYSTEMS {
                match self.fw_system_occupations.get_mut(&sys_id) {
                    Some(occ) => {
                        if occ.victory_points_threshold == 3_000 || occ.victory_points_threshold == 0 {
                            occ.victory_points_threshold = 75_000;
                        }
                    }
                    None => {
                        self.fw_system_occupations.insert(
                            sys_id,
                            FwSystemOccupationRecord {
                                solar_system_id: sys_id,
                                owner_faction_id: owner,
                                occupier_faction_id: owner,
                                victory_points: 0,
                                victory_points_threshold: 75_000,
                            },
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backfill_empty_populates_all_175_systems_at_75k() {
        let mut snap = WorldStateSnapshot::default();
        assert!(snap.fw_system_occupations.is_empty());
        snap.backfill_fw_system_occupations();
        assert_eq!(snap.fw_system_occupations.len(), 175);
        for &(sys_id, owner) in DEFAULT_FW_WARZONE_SYSTEMS {
            let occ = snap.fw_system_occupations.get(&sys_id).expect("system present");
            assert_eq!(occ.owner_faction_id, owner);
            assert_eq!(occ.occupier_faction_id, owner);
            assert_eq!(occ.victory_points, 0);
            assert_eq!(occ.victory_points_threshold, 75_000);
        }
    }

    #[test]
    fn test_backfill_migrates_legacy_3000_threshold_to_75k() {
        let mut snap = WorldStateSnapshot::default();
        snap.fw_system_occupations.insert(
            30002055,
            FwSystemOccupationRecord {
                solar_system_id: 30002055,
                owner_faction_id: 500002,
                occupier_faction_id: 500002,
                victory_points: 1_200,
                victory_points_threshold: 3_000,
            },
        );
        snap.backfill_fw_system_occupations();
        // 30002055 should have threshold updated to 75_000 and keep victory_points 1_200
        let amo = snap.fw_system_occupations.get(&30002055).unwrap();
        assert_eq!(amo.victory_points, 1_200);
        assert_eq!(amo.victory_points_threshold, 75_000);
        // Missing systems should also be backfilled to 175 total
        assert_eq!(snap.fw_system_occupations.len(), 175);
    }
}
