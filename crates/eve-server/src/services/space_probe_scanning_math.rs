//! Pure scanner probe math, range/deviation calculations, dogma state evaluation,
//! and signature difficulty lookup extracted from `space_probe_scanning`.

#![forbid(unsafe_code)]

use eve_db::types::CosmicSignatureRecord;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};

// parity: feat:scanning.probe-target-classes, feat:scanning.probe-range-deviation
pub fn is_combat_scanner_probe(sde: &SdeRegistry, probe_type_id: u32) -> bool {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1413).copied())
        .unwrap_or_else(|| sde.attributes.get(&1413).map(|a| a.default_value).unwrap_or(0.0))
        >= 0.5
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_scan_range_au(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1370).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1370).map(|a| a.default_value.max(0.25)).unwrap_or(0.25))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_allowed_ranges_au(sde: &SdeRegistry, probe_type_id: u32) -> [f64; 8] {
    let base = probe_base_scan_range_au(sde, probe_type_id);
    std::array::from_fn(|i| base * 2.0_f64.powi(i as i32))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_max_deviation_au(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1372).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1372).map(|a| a.default_value.max(0.125)).unwrap_or(0.125))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_sensor_strength(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1371).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1371).map(|a| a.default_value.max(1.0)).unwrap_or(40.0))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_max_flight_time_s(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&281).copied())
        .filter(|&v| v > 0.0)
        .map(|ms| ms / 1000.0)
        .unwrap_or_else(|| sde.attributes.get(&281).map(|a| (a.default_value / 1000.0).max(1.0)).unwrap_or(4_000.0))
}

// parity: feat:scanning.probe-range-deviation
pub fn compute_probe_max_deviation_au(
    base_deviation_au: f64,
    range_au: f64,
    base_range_au: f64,
    astrometrics_level: u8,
    pinpointing_level: u8,
) -> f64 {
    let astro_factor = 1.0 - 0.05 * (astrometrics_level.min(5) as f64);
    let pinpoint_factor = 1.0 - 0.05 * (pinpointing_level.min(5) as f64);
    base_deviation_au * (range_au / base_range_au.max(0.01)) * astro_factor * pinpoint_factor
}

pub(super) fn evaluate_probe_dogma_state(
    fit_opt: Option<&eve_dogma::ShipFittingEngine>,
    probe_id: i64,
    probe_type_id: u32,
    sde: &SdeRegistry,
) -> Option<eve_dogma::DogmaItemState> {
    let fit = fit_opt?;
    if let Some(st) = fit.drone_state(probe_id, sde) {
        if st.type_id == probe_type_id {
            return Some(st);
        }
    }
    if let Some(st) = fit
        .evaluated_drones
        .values()
        .chain(fit.evaluated_charges.values())
        .find(|s| s.type_id == probe_type_id)
    {
        return Some(st.clone());
    }
    if sde.types.contains_key(&probe_type_id) {
        let mut tmp = fit.clone();
        tmp.add_drone(probe_id, probe_type_id, sde);
        return tmp.drone_state(probe_id, sde);
    }
    None
}

// parity: feat:scanning.signature-strength-levels
pub fn signature_difficulty_level_and_min_strength(
    sde: &SdeRegistry,
    _sys_id: SolarSystemId,
    sig: &CosmicSignatureRecord,
) -> (u8, f64) {
    let target_name = sig.type_name.trim();
    let level = sde
        .dungeons
        .values()
        .find(|d| d.name.trim().eq_ignore_ascii_case(target_name))
        .and_then(|d| d.difficulty)
        .map(|diff| (diff as u8).clamp(1, 5))
        .unwrap_or(1);
    let min_strength = match level {
        1..=3 => 42.0,
        4 => 72.0,
        _ => 104.0,
    };
    (level, min_strength)
}

/// Formats `OnSystemScanStarted` probe payload as the V24 client's `{probeID: KeyVal(pos=(x, y, z), state=4)}` Dict.
pub(super) fn probe_scan_started_probes_dict(probes: &[(i64, [f64; 3])]) -> PyRep {
    PyRep::Dict(
        probes
            .iter()
            .map(|&(pid, pos)| {
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        (
                            "pos",
                            PyRep::Tuple(vec![
                                PyRep::Real(pos[0]),
                                PyRep::Real(pos[1]),
                                PyRep::Real(pos[2]),
                            ]),
                        ),
                        ("state", PyRep::Integer(4)),
                    ]),
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_scan_started_probes_dict_encodes_pos_tuple_and_scanning_state() {
        let rep = probe_scan_started_probes_dict(&[(901, [12.5, -34.0, 56.25])]);
        let PyRep::Dict(entries) = rep else {
            panic!("expected Dict");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, PyRep::Integer(901));
        assert_eq!(entries[0].1.dict_get("state"), Some(&PyRep::Integer(4)));
        assert_eq!(
            entries[0].1.dict_get("pos"),
            Some(&PyRep::Tuple(vec![
                PyRep::Real(12.5),
                PyRep::Real(-34.0),
                PyRep::Real(56.25),
            ]))
        );
    }
}
