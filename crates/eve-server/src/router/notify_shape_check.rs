//! Strict recursive runtime shape checker for typed and registered notifications (`#933` Track 6b).
//!
//! Compiled only under `#[cfg(any(test, feature = "testkit"))]` so non-test release builds incur
//! zero runtime overhead. Every production typed or registered notification emit path validates
//! emitted `Vec<PyRep>` arguments against the event's registered `sample_payloads`.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use std::collections::{BTreeSet, HashMap};

use super::notify_registry::{
    all_registered_notifications, NotificationRegistryEntry, TypedNotification,
};

const MAX_SHAPE_DEPTH: usize = 64;
const MAX_SUMMARY_DEPTH: usize = 3;
const MAX_SUMMARY_ITEMS: usize = 6;

/// Registered notification events whose `sample_payloads` intentionally rely on a documented
/// pre-`client_compat` legacy encoding (Track 7+) that conflicts with strict `client_contract`
/// sample checks (vacuous-collection or legacy arity truncated by `client_compat`). Checked strictly
/// for registered arity (`min_arity..=max_arity`) and top-level `PyRep` discriminants; monotonically
/// shrink-only toward `0`.
pub const INCOMPLETE_SAMPLE_EVENTS: &[&str] = &[
    "OnSharedBookmarksFolderUpdated",
    "OnTradeMoneyOffer",
    "OnTradeOffer",
];

/// Explicit top-level `PyRep` discriminants for allowlisted trailing arguments beyond the base
/// sample length (indexed from `base_sample_len`), so every positional slot `0..args.len()` is
/// strictly checked even when pre-`client_compat` trailing args extend past the V24 sample.
pub(crate) const INCOMPLETE_TRAILING_SLOT_DISCRIMINANTS: &[(&str, &[&[&str]])] = &[
    ("OnSharedBookmarksFolderUpdated", &[]),
    ("OnTradeMoneyOffer", &[&["Real"]]),
    ("OnTradeOffer", &[&["Bool"]]),
];

/// Registered notification events whose `sample_payloads()` hold canonical V24 shapes while an
/// out-of-budget/overlapping `6b-2` emitter still temporarily sends a transitional payload on ONE
/// documented slot (`Refs #933`). Non-deferred slots remain strictly recursively validated; deleted
/// (`-> 0`) in Track `6b-2`.
pub const DEFERRED_6B2_EMITTER_COMPAT_EVENTS: &[&str] = &[
    "OnDeathzoneStateUpdate",
    "OnFighterAddedToController",
    "OnMissionsUpdated",
    "OnProgressionTaskWidgetUpdate",
    "OnTaleData",
];

/// Registered events whose `PyRep::Dict` payloads legitimately use dynamic runtime string keys
/// (e.g., anomaly/signature IDs, localization formatting kwargs, or node-graph blackboard keys)
/// rather than a closed fixed string key schema. Fixed-schema dicts (`OnSessionChanged`,
/// `OnCombatMessage`, `OnDamageMessage`, `OnDamageMessages`, `OnOreMined`,
/// `OnSystemESSDataChanged`, `OnMainBankPlayerLinkDisconnected`, `OnBountyAddedToPayout`, etc.)
/// must NEVER be added here.
pub const DYNAMIC_STRING_MAP_EVENTS: &[&str] = &[
    "OnClientNodeGraphUpdate",
    "OnEffectMessage",
    "OnNotificationReceived",
    "OnRemoteMessage",
    "OnSignalTrackerAnomalyUpdate",
    "OnSignalTrackerFullState",
    "OnSignalTrackerSignatureUpdate",
    "OnStartClientNodeGraph",
];

/// Returns `true` if `event` is on the shrink-only incomplete-sample allowlist.
pub fn is_incomplete_sample_event(event: &str) -> bool {
    INCOMPLETE_SAMPLE_EVENTS.contains(&event)
}

fn is_deferred_6b2_compat_event(event: &str) -> bool {
    DEFERRED_6B2_EMITTER_COMPAT_EVENTS.contains(&event)
}

fn is_dynamic_string_map_event(event: &str) -> bool {
    DYNAMIC_STRING_MAP_EVENTS.contains(&event)
}

/// Returns the canonical `PyRep` discriminant variant name.
pub fn discriminant_name(rep: &PyRep) -> &'static str {
    match rep {
        PyRep::None => "None",
        PyRep::Bool(_) => "Bool",
        PyRep::Integer(_) => "Integer",
        PyRep::Real(_) => "Real",
        PyRep::Buffer(_) => "Buffer",
        PyRep::Bytes(_) => "Bytes",
        PyRep::Token(_) => "Token",
        PyRep::String(_) => "String",
        PyRep::Tuple(_) => "Tuple",
        PyRep::List(_) => "List",
        PyRep::Dict(_) => "Dict",
        PyRep::PySet(_) => "PySet",
        PyRep::Set(_) => "Set",
        PyRep::KeyVal(_) => "KeyVal",
        PyRep::Rowset { .. } => "Rowset",
        PyRep::IndexRowset { .. } => "IndexRowset",
        PyRep::TupleSet { .. } => "TupleSet",
        PyRep::DBRow { .. } => "DBRow",
        PyRep::Picked(_) => "Picked",
        PyRep::Object { .. } => "Object",
        PyRep::ObjectEx { .. } => "ObjectEx",
        PyRep::SubStruct(_) => "SubStruct",
        PyRep::ChecksumedStream { .. } => "ChecksumedStream",
        PyRep::SubStream(_) => "SubStream",
    }
}

/// Produces a deterministic, concise structural summary of a `PyRep` value for diagnostics.
pub fn summarize_pyrep_shape(rep: &PyRep) -> String {
    summarize_pyrep_shape_inner(rep, 0)
}

fn summarize_pyrep_shape_inner(rep: &PyRep, depth: usize) -> String {
    if depth >= MAX_SUMMARY_DEPTH {
        return discriminant_name(rep).to_string();
    }
    match rep {
        PyRep::None
        | PyRep::Bool(_)
        | PyRep::Integer(_)
        | PyRep::Real(_)
        | PyRep::Buffer(_)
        | PyRep::Bytes(_)
        | PyRep::Token(_)
        | PyRep::String(_)
        | PyRep::Picked(_) => discriminant_name(rep).to_string(),
        PyRep::Tuple(items) => {
            let parts: Vec<String> = items
                .iter()
                .take(MAX_SUMMARY_ITEMS)
                .map(|el| summarize_pyrep_shape_inner(el, depth + 1))
                .collect();
            let suffix = if items.len() > MAX_SUMMARY_ITEMS {
                ", ..."
            } else {
                ""
            };
            format!("Tuple({})[{}{}]", items.len(), parts.join(", "), suffix)
        }
        PyRep::List(items) | PyRep::Set(items) | PyRep::PySet(items) => {
            let kind = discriminant_name(rep);
            if items.is_empty() {
                return format!("{kind}([])");
            }
            let mut uniq = BTreeSet::new();
            for el in items.iter().take(MAX_SUMMARY_ITEMS) {
                uniq.insert(summarize_pyrep_shape_inner(el, depth + 1));
            }
            let joined: Vec<String> = uniq.into_iter().collect();
            format!("{kind}(len={}: [{}])", items.len(), joined.join(" | "))
        }
        PyRep::Dict(entries) => {
            if entries.is_empty() {
                return "Dict({})".to_string();
            }
            let parts: Vec<String> = entries
                .iter()
                .take(MAX_SUMMARY_ITEMS)
                .map(|(k, v)| {
                    let k_str = match k {
                        PyRep::String(s) => format!("String({s:?})"),
                        _ => summarize_pyrep_shape_inner(k, depth + 1),
                    };
                    format!("{k_str}: {}", summarize_pyrep_shape_inner(v, depth + 1))
                })
                .collect();
            let suffix = if entries.len() > MAX_SUMMARY_ITEMS {
                ", ..."
            } else {
                ""
            };
            format!("Dict(len={}: {{{}{}}})", entries.len(), parts.join(", "), suffix)
        }
        PyRep::KeyVal(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .take(MAX_SUMMARY_ITEMS)
                .map(|k| format!("{k}: {}", summarize_pyrep_shape_inner(&map[*k], depth + 1)))
                .collect();
            let suffix = if keys.len() > MAX_SUMMARY_ITEMS {
                ", ..."
            } else {
                ""
            };
            format!("KeyVal({{{}{}}})", parts.join(", "), suffix)
        }
        PyRep::Rowset { columns, rows } => {
            format!("Rowset(cols={columns:?}, rows={})", rows.len())
        }
        PyRep::TupleSet { columns, rows } => {
            format!("TupleSet(cols={columns:?}, rows={})", rows.len())
        }
        PyRep::IndexRowset {
            key_column,
            columns,
            items,
        } => format!(
            "IndexRowset(key={key_column:?}, cols={columns:?}, items={})",
            items.len()
        ),
        PyRep::DBRow { columns, .. } => {
            let names: Vec<&str> = columns.iter().map(|(n, _)| n.as_str()).collect();
            format!("DBRow(cols={names:?})")
        }
        PyRep::Object { type_name, args } => {
            format!(
                "Object({type_name:?}, {})",
                summarize_pyrep_shape_inner(args, depth + 1)
            )
        }
        PyRep::ObjectEx {
            is_type_2,
            header,
            list,
            dict,
        } => format!(
            "ObjectEx(type2={is_type_2}, header={}, list_len={}, dict_len={})",
            summarize_pyrep_shape_inner(header, depth + 1),
            list.len(),
            dict.len()
        ),
        PyRep::SubStruct(inner) => {
            format!("SubStruct({})", summarize_pyrep_shape_inner(inner, depth + 1))
        }
        PyRep::ChecksumedStream { body, .. } => format!(
            "ChecksumedStream({})",
            summarize_pyrep_shape_inner(body, depth + 1)
        ),
        PyRep::SubStream(inner) => {
            format!("SubStream({})", summarize_pyrep_shape_inner(inner, depth + 1))
        }
    }
}

/// Formats an argument slice as a concise shape list for diagnostics.
pub fn summarize_args_shape(args: &[PyRep]) -> String {
    let parts: Vec<String> = args.iter().map(summarize_pyrep_shape).collect();
    format!("[{}]", parts.join(", "))
}

fn summarize_samples_list(samples: &[Vec<PyRep>]) -> String {
    let parts: Vec<String> = samples
        .iter()
        .take(4)
        .enumerate()
        .map(|(i, s)| format!("sample[{i}]={}", summarize_args_shape(s)))
        .collect();
    let suffix = if samples.len() > 4 { ", ..." } else { "" };
    format!("{}{}", parts.join("; "), suffix)
}

fn match_collection_elements(
    event: &str,
    kind: &str,
    actual: &[PyRep],
    sample: &[PyRep],
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if actual.is_empty() {
        return Ok(());
    }
    if sample.is_empty() {
        return Err(format!(
            "{path}: non-empty {kind} (len={}) does not match empty sample {kind}([])",
            actual.len()
        ));
    }
    for (idx, item) in actual.iter().enumerate() {
        let item_path = format!("{path}[{idx}]");
        if !sample
            .iter()
            .any(|s_item| match_pyrep_recursive(event, item, s_item, &item_path, depth + 1).is_ok())
        {
            let expected: Vec<String> = sample.iter().map(summarize_pyrep_shape).collect();
            return Err(format!(
                "{item_path}: {kind} element {} does not match sample element shape(s) [{}]",
                summarize_pyrep_shape(item),
                expected.join(" | ")
            ));
        }
    }
    Ok(())
}

fn match_dict_entries(
    event: &str,
    actual: &[(PyRep, PyRep)],
    sample: &[(PyRep, PyRep)],
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if actual.is_empty() {
        return Ok(());
    }
    if sample.is_empty() {
        return Err(format!(
            "{path}: non-empty Dict (len={}) does not match empty sample Dict({{}})",
            actual.len()
        ));
    }
    let sample_has_string_keys = sample
        .iter()
        .any(|(sk, _)| matches!(sk, PyRep::String(_)));
    for (idx, (ak, av)) in actual.iter().enumerate() {
        if let PyRep::String(k_str) = ak {
            let fixed_val_samples: Vec<&PyRep> = sample
                .iter()
                .filter_map(|(sk, sv)| matches!(sk, PyRep::String(s) if s == k_str).then_some(sv))
                .collect();
            if !fixed_val_samples.is_empty() {
                let val_path = format!("{path}[{k_str:?}]");
                if !fixed_val_samples
                    .iter()
                    .any(|sv| match_pyrep_recursive(event, av, sv, &val_path, depth + 1).is_ok())
                {
                    let expected: Vec<String> = fixed_val_samples
                        .iter()
                        .map(|sv| summarize_pyrep_shape(sv))
                        .collect();
                    return Err(format!(
                        "{val_path}: Dict value {} does not match expected shape(s) [{}] for fixed string key {k_str:?}",
                        summarize_pyrep_shape(av),
                        expected.join(" | ")
                    ));
                }
                continue;
            }
            if sample_has_string_keys && !is_dynamic_string_map_event(event) {
                let mut expected_keys: Vec<&str> = sample
                    .iter()
                    .filter_map(|(sk, _)| sk.as_str())
                    .collect();
                expected_keys.sort();
                expected_keys.dedup();
                return Err(format!(
                    "{path}[{k_str:?}]: unexpected Dict string key {k_str:?} for fixed-schema event '{event}' (expected keys: {expected_keys:?})"
                ));
            }
        }
        let key_path = format!("{path}[key#{idx}]");
        let val_path = format!("{path}[val#{idx}]");
        if !sample.iter().any(|(sk, sv)| {
            match_pyrep_recursive(event, ak, sk, &key_path, depth + 1).is_ok()
                && match_pyrep_recursive(event, av, sv, &val_path, depth + 1).is_ok()
        }) {
            let expected: Vec<String> = sample
                .iter()
                .map(|(sk, sv)| format!("({}, {})", summarize_pyrep_shape(sk), summarize_pyrep_shape(sv)))
                .collect();
            return Err(format!(
                "{path}[entry#{idx}]: Dict pair ({}, {}) does not match any sample pair [{}]",
                summarize_pyrep_shape(ak),
                summarize_pyrep_shape(av),
                expected.join(" | ")
            ));
        }
    }
    Ok(())
}

fn match_keyval_fields(
    event: &str,
    actual: &HashMap<String, PyRep>,
    sample: &HashMap<String, PyRep>,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if sample.is_empty() && !actual.is_empty() {
        return Err(format!(
            "{path}: non-empty KeyVal (len={}) does not match empty sample KeyVal({{}})",
            actual.len()
        ));
    }
    let mut extra_keys: Vec<&String> = actual
        .keys()
        .filter(|k| !sample.contains_key(k.as_str()))
        .collect();
    if !extra_keys.is_empty() {
        extra_keys.sort();
        let mut expected_keys: Vec<&String> = sample.keys().collect();
        expected_keys.sort();
        return Err(format!(
            "{path}.{}: KeyVal contains unexpected field(s) {:?} absent from sample (expected fields: {expected_keys:?})",
            extra_keys[0],
            extra_keys
        ));
    }
    let mut keys: Vec<&String> = sample.keys().collect();
    keys.sort();
    for k in keys {
        let sv = &sample[k];
        let field_path = format!("{path}.{k}");
        let Some(av) = actual.get(k) else {
            return Err(format!(
                "{field_path}: KeyVal missing sample field '{k}' (expected {})",
                summarize_pyrep_shape(sv)
            ));
        };
        match_pyrep_recursive(event, av, sv, &field_path, depth + 1)?;
    }
    Ok(())
}

fn match_row_against_samples(
    event: &str,
    kind: &str,
    row_idx: usize,
    actual_row: &[PyRep],
    expected_col_count: usize,
    sample_rows: &[Vec<PyRep>],
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if actual_row.len() != expected_col_count {
        return Err(format!(
            "{path}.rows[{row_idx}]: {kind} row length {} != column count {expected_col_count}",
            actual_row.len()
        ));
    }
    let row_matched = sample_rows.iter().any(|srow| {
        srow.len() == actual_row.len()
            && actual_row.iter().enumerate().all(|(c_idx, cell)| {
                match_pyrep_recursive(
                    event,
                    cell,
                    &srow[c_idx],
                    &format!("{path}.rows[{row_idx}][{c_idx}]"),
                    depth + 1,
                )
                .is_ok()
            })
    });
    if !row_matched {
        return Err(format!(
            "{path}.rows[{row_idx}]: {kind} row {} does not match any sample row",
            summarize_args_shape(actual_row)
        ));
    }
    Ok(())
}

fn match_tabular_rep(
    event: &str,
    kind: &str,
    ac: &[String],
    ar: &[Vec<PyRep>],
    sc: &[String],
    sr: &[Vec<PyRep>],
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if ac != sc {
        return Err(format!(
            "{path}: {kind} columns mismatch: got {ac:?}, expected {sc:?}"
        ));
    }
    if ar.is_empty() {
        return Ok(());
    }
    if sr.is_empty() {
        return Err(format!(
            "{path}: non-empty {kind} (rows={}) does not match empty sample {kind}",
            ar.len()
        ));
    }
    for (r_idx, row) in ar.iter().enumerate() {
        match_row_against_samples(event, kind, r_idx, row, ac.len(), sr, path, depth)?;
    }
    Ok(())
}

fn match_pyrep_recursive(
    event: &str,
    actual: &PyRep,
    sample: &PyRep,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_SHAPE_DEPTH {
        return Err(format!(
            "{path}: exceeded maximum shape recursion depth ({MAX_SHAPE_DEPTH})"
        ));
    }
    match (actual, sample) {
        (PyRep::None, PyRep::None)
        | (PyRep::Bool(_), PyRep::Bool(_))
        | (PyRep::Integer(_), PyRep::Integer(_))
        | (PyRep::Real(_), PyRep::Real(_))
        | (PyRep::Buffer(_), PyRep::Buffer(_))
        | (PyRep::Bytes(_), PyRep::Bytes(_))
        | (PyRep::Token(_), PyRep::Token(_))
        | (PyRep::String(_), PyRep::String(_))
        | (PyRep::Picked(_), PyRep::Picked(_)) => Ok(()),
        (PyRep::Tuple(a), PyRep::Tuple(s)) => {
            if a.len() != s.len() {
                return Err(format!(
                    "{path}: Tuple length mismatch: got {}, expected {} (actual={}, expected={})",
                    a.len(),
                    s.len(),
                    summarize_pyrep_shape(actual),
                    summarize_pyrep_shape(sample)
                ));
            }
            for (idx, (ae, se)) in a.iter().zip(s.iter()).enumerate() {
                match_pyrep_recursive(event, ae, se, &format!("{path}[{idx}]"), depth + 1)?;
            }
            Ok(())
        }
        (PyRep::List(a), PyRep::List(s)) => {
            match_collection_elements(event, "List", a, s, path, depth)
        }
        (PyRep::Set(a), PyRep::Set(s)) => {
            match_collection_elements(event, "Set", a, s, path, depth)
        }
        (PyRep::PySet(a), PyRep::PySet(s)) => {
            match_collection_elements(event, "PySet", a, s, path, depth)
        }
        (PyRep::Dict(a), PyRep::Dict(s)) => match_dict_entries(event, a, s, path, depth),
        (PyRep::KeyVal(a), PyRep::KeyVal(s)) => match_keyval_fields(event, a, s, path, depth),
        (
            PyRep::Rowset {
                columns: ac,
                rows: ar,
            },
            PyRep::Rowset {
                columns: sc,
                rows: sr,
            },
        ) => match_tabular_rep(event, "Rowset", ac, ar, sc, sr, path, depth),
        (
            PyRep::TupleSet {
                columns: ac,
                rows: ar,
            },
            PyRep::TupleSet {
                columns: sc,
                rows: sr,
            },
        ) => match_tabular_rep(event, "TupleSet", ac, ar, sc, sr, path, depth),
        (
            PyRep::IndexRowset {
                key_column: ak,
                columns: ac,
                items: ai,
            },
            PyRep::IndexRowset {
                key_column: sk,
                columns: sc,
                items: si,
            },
        ) => {
            if ak != sk || ac != sc {
                return Err(format!(
                    "{path}: IndexRowset schema mismatch: got (key={ak:?}, cols={ac:?}), expected (key={sk:?}, cols={sc:?})"
                ));
            }
            if ai.is_empty() {
                return Ok(());
            }
            if si.is_empty() {
                return Err(format!(
                    "{path}: non-empty IndexRowset (items={}) does not match empty sample IndexRowset",
                    ai.len()
                ));
            }
            for (idx, (k, row)) in ai.iter().enumerate() {
                if row.len() != ac.len() {
                    return Err(format!(
                        "{path}.items[{idx}]: IndexRowset row length {} != column count {}",
                        row.len(),
                        ac.len()
                    ));
                }
                let matched = si.iter().any(|(sk_i, srow_i)| {
                    srow_i.len() == row.len()
                        && match_pyrep_recursive(
                            event,
                            k,
                            sk_i,
                            &format!("{path}.items[{idx}].key"),
                            depth + 1,
                        )
                        .is_ok()
                        && row.iter().enumerate().all(|(c_idx, cell)| {
                            match_pyrep_recursive(
                                event,
                                cell,
                                &srow_i[c_idx],
                                &format!("{path}.items[{idx}][{c_idx}]"),
                                depth + 1,
                            )
                            .is_ok()
                        })
                });
                if !matched {
                    return Err(format!(
                        "{path}.items[{idx}]: IndexRowset entry ({}, {}) does not match any sample item",
                        summarize_pyrep_shape(k),
                        summarize_args_shape(row)
                    ));
                }
            }
            Ok(())
        }
        (
            PyRep::DBRow {
                header: ah,
                columns: ac,
                ..
            },
            PyRep::DBRow {
                header: sh,
                columns: sc,
                ..
            },
        ) => {
            match_pyrep_recursive(event, ah, sh, &format!("{path}.header"), depth + 1)?;
            if ac.len() != sc.len() {
                return Err(format!(
                    "{path}: DBRow column count mismatch: got {}, expected {}",
                    ac.len(),
                    sc.len()
                ));
            }
            for ((an, av), (sn, sv)) in ac.iter().zip(sc.iter()) {
                if an != sn {
                    return Err(format!(
                        "{path}: DBRow column name mismatch: got {an:?}, expected {sn:?}"
                    ));
                }
                match_pyrep_recursive(event, av, sv, &format!("{path}.{an}"), depth + 1)?;
            }
            Ok(())
        }
        (
            PyRep::Object {
                type_name: at,
                args: aa,
            },
            PyRep::Object {
                type_name: st,
                args: sa,
            },
        ) => {
            if at != st {
                return Err(format!(
                    "{path}: Object type_name mismatch: got {at:?}, expected {st:?}"
                ));
            }
            match_pyrep_recursive(event, aa, sa, &format!("{path}<{at}>"), depth + 1)
        }
        (
            PyRep::ObjectEx {
                is_type_2: at2,
                header: ah,
                list: al,
                dict: ad,
            },
            PyRep::ObjectEx {
                is_type_2: st2,
                header: sh,
                list: sl,
                dict: sd,
            },
        ) => {
            if at2 != st2 {
                return Err(format!(
                    "{path}: ObjectEx is_type_2 mismatch: got {at2}, expected {st2}"
                ));
            }
            match_pyrep_recursive(event, ah, sh, &format!("{path}.header"), depth + 1)?;
            match_collection_elements(event, "ObjectEx.list", al, sl, &format!("{path}.list"), depth)?;
            match_dict_entries(event, ad, sd, &format!("{path}.dict"), depth)
        }
        (PyRep::SubStruct(a), PyRep::SubStruct(s)) => {
            match_pyrep_recursive(event, a, s, &format!("{path}.SubStruct"), depth + 1)
        }
        (PyRep::ChecksumedStream { body: a, .. }, PyRep::ChecksumedStream { body: s, .. }) => {
            match_pyrep_recursive(event, a, s, &format!("{path}.ChecksumedStream"), depth + 1)
        }
        (PyRep::SubStream(a), PyRep::SubStream(s)) => {
            match_pyrep_recursive(event, a, s, &format!("{path}.SubStream"), depth + 1)
        }
        (a, s) => Err(format!(
            "{path}: expected {}, got {}",
            summarize_pyrep_shape(s),
            summarize_pyrep_shape(a)
        )),
    }
}

fn check_shared_bookmarks_folder_updated(
    samples: &[Vec<PyRep>],
    args: &[PyRep],
) -> Result<(), String> {
    let event = "OnSharedBookmarksFolderUpdated";
    if args.len() != 1 {
        return Err(format!(
            "Notification shape mismatch for event '{event}': expected 1 arg, got {}",
            args.len()
        ));
    }
    let PyRep::List(entries) = &args[0] else {
        return Err(format!(
            "Notification shape mismatch for allowlisted event '{event}': arg[0] expected List, got {}",
            summarize_pyrep_shape(&args[0])
        ));
    };
    let mut part2_candidates: Vec<PyRep> = samples
        .iter()
        .filter_map(|s| match s.first() {
            Some(PyRep::List(items)) => items.first().and_then(|it| match it {
                PyRep::Tuple(parts) if parts.len() == 3 => Some(parts[2].clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    part2_candidates.push(PyRep::Integer(1));
    part2_candidates.push(PyRep::Bool(false));
    part2_candidates.push(PyRep::List(vec![PyRep::keyval([(
        "bookmarkID",
        PyRep::Integer(1),
    )])]));

    let sample_folder_key = PyRep::Tuple(vec![PyRep::Integer(1)]);
    let sample_action = PyRep::String("FolderChanged".into());
    for (idx, entry) in entries.iter().enumerate() {
        let PyRep::Tuple(parts) = entry else {
            return Err(format!(
                "Notification shape mismatch for allowlisted event '{event}' at arg[0][{idx}]: expected 3-tuple (folder_key, action, payload), got {}",
                summarize_pyrep_shape(entry)
            ));
        };
        if parts.len() != 3 {
            return Err(format!(
                "Notification shape mismatch for allowlisted event '{event}' at arg[0][{idx}]: expected Tuple(3), got Tuple({})",
                parts.len()
            ));
        }
        match_pyrep_recursive(
            event,
            &parts[0],
            &sample_folder_key,
            &format!("arg[0][{idx}][0]"),
            1,
        )?;
        match_pyrep_recursive(
            event,
            &parts[1],
            &sample_action,
            &format!("arg[0][{idx}][1]"),
            1,
        )?;
        let part2_path = format!("arg[0][{idx}][2]");
        if !part2_candidates
            .iter()
            .any(|cand| match_pyrep_recursive(event, &parts[2], cand, &part2_path, 1).is_ok())
        {
            return Err(format!(
                "Notification shape mismatch for allowlisted event '{event}' at {part2_path}: got {}, expected bookmark update payload or legacy FolderChanged Integer/Bool",
                summarize_pyrep_shape(&parts[2])
            ));
        }
    }
    Ok(())
}

fn check_deferred_6b2_compat(
    event: &str,
    samples: &[Vec<PyRep>],
    args: &[PyRep],
) -> Result<(), String> {
    let same_len: Vec<&Vec<PyRep>> = samples.iter().filter(|s| s.len() == args.len()).collect();
    if same_len.is_empty() {
        return Err(format!(
            "Notification shape mismatch for deferred compat event '{event}': unexpected arg count {}; actual={}",
            args.len(),
            summarize_args_shape(args)
        ));
    }
    match event {
        "OnFighterAddedToController" => {
            for idx in 0..4 {
                if !same_len.iter().any(|s| {
                    match_pyrep_recursive(event, &args[idx], &s[idx], &format!("arg[{idx}]"), 0)
                        .is_ok()
                }) {
                    return Err(format!(
                        "Notification shape mismatch for deferred compat event '{event}' at arg[{idx}]: got {}, expected {}",
                        summarize_pyrep_shape(&args[idx]),
                        summarize_pyrep_shape(&same_len[0][idx])
                    ));
                }
            }
            if matches!(args[4], PyRep::List(_))
                || same_len
                    .iter()
                    .any(|s| match_pyrep_recursive(event, &args[4], &s[4], "arg[4]", 0).is_ok())
            {
                return Ok(());
            }
            Err(format!(
                "Notification shape mismatch for deferred compat event '{event}' at arg[4]: expected Dict or transitional empty List, got {}",
                summarize_pyrep_shape(&args[4])
            ))
        }
        "OnDeathzoneStateUpdate" => {
            for idx in 0..2 {
                match_pyrep_recursive(
                    event,
                    &args[idx],
                    &same_len[0][idx],
                    &format!("arg[{idx}]"),
                    0,
                )?;
            }
            let legacy_dict = PyRep::Dict(vec![
                (PyRep::String("timeRemainingMs".into()), PyRep::Real(15_000.0)),
                (PyRep::String("dangerLevel".into()), PyRep::Real(1.0)),
            ]);
            if same_len
                .iter()
                .any(|s| match_pyrep_recursive(event, &args[2], &s[2], "arg[2]", 0).is_ok())
                || match_pyrep_recursive(event, &args[2], &legacy_dict, "arg[2]", 0).is_ok()
            {
                return Ok(());
            }
            Err(format!(
                "Notification shape mismatch for deferred compat event '{event}' at arg[2]: expected None, Deathzone KeyVal, or legacy abyssal Dict, got {}",
                summarize_pyrep_shape(&args[2])
            ))
        }
        "OnTaleData" => {
            match_pyrep_recursive(event, &args[0], &same_len[0][0], "arg[0]", 0)?;
            let PyRep::Dict(entries) = &args[1] else {
                return Err(format!(
                    "Notification shape mismatch for deferred compat event '{event}' at arg[1]: expected Dict, got {}",
                    summarize_pyrep_shape(&args[1])
                ));
            };
            let canonical_val = match &same_len[0][1] {
                PyRep::Dict(pairs) => pairs.first().map(|(_, v)| v.clone()),
                _ => None,
            };
            for (idx, (k, v)) in entries.iter().enumerate() {
                match_pyrep_recursive(
                    event,
                    k,
                    &PyRep::Integer(701),
                    &format!("arg[1][key#{idx}]"),
                    1,
                )?;
                let dict_ok = canonical_val.as_ref().is_some_and(|cv| {
                    match_pyrep_recursive(event, v, cv, &format!("arg[1][val#{idx}]"), 1).is_ok()
                });
                let legacy_kv_ok = matches!(
                    v,
                    PyRep::KeyVal(kv) if matches!(kv.get("taleID"), Some(PyRep::Integer(_)))
                );
                if !dict_ok && !legacy_kv_ok {
                    return Err(format!(
                        "Notification shape mismatch for deferred compat event '{event}' at arg[1][val#{idx}]: expected incursion Dict or transitional tale KeyVal with integer taleID, got {}",
                        summarize_pyrep_shape(v)
                    ));
                }
            }
            Ok(())
        }
        "OnMissionsUpdated" => {
            let PyRep::List(entries) = &args[0] else {
                return Err(format!(
                    "Notification shape mismatch for deferred compat event '{event}' at arg[0]: expected List, got {}",
                    summarize_pyrep_shape(&args[0])
                ));
            };
            let canonical_dict = match &same_len[0][0] {
                PyRep::List(items) => items.first().cloned(),
                _ => None,
            };
            let legacy_kv = canonical_dict.as_ref().and_then(|d| match d {
                PyRep::Dict(pairs) => Some(PyRep::KeyVal(
                    pairs
                        .iter()
                        .filter_map(|(k, v)| k.as_str().map(|ks| (ks.to_string(), v.clone())))
                        .collect(),
                )),
                _ => None,
            });
            for (idx, item) in entries.iter().enumerate() {
                let path = format!("arg[0][{idx}]");
                let dict_ok = canonical_dict
                    .as_ref()
                    .is_some_and(|cd| match_pyrep_recursive(event, item, cd, &path, 1).is_ok());
                let kv_ok = legacy_kv
                    .as_ref()
                    .is_some_and(|lk| match_pyrep_recursive(event, item, lk, &path, 1).is_ok());
                if !dict_ok && !kv_ok {
                    return Err(format!(
                        "Notification shape mismatch for deferred compat event '{event}' at {path}: expected mission Dict or transitional mission KeyVal, got {}",
                        summarize_pyrep_shape(item)
                    ));
                }
            }
            Ok(())
        }
        "OnProgressionTaskWidgetUpdate" => {
            for idx in 0..2 {
                match_pyrep_recursive(
                    event,
                    &args[idx],
                    &same_len[0][idx],
                    &format!("arg[{idx}]"),
                    0,
                )?;
            }
            if matches!(args[2], PyRep::Integer(_) | PyRep::String(_))
                || same_len
                    .iter()
                    .any(|s| match_pyrep_recursive(event, &args[2], &s[2], "arg[2]", 0).is_ok())
            {
                return Ok(());
            }
            Err(format!(
                "Notification shape mismatch for deferred compat event '{event}' at arg[2]: expected widget_state Dict or transitional Integer/String, got {}",
                summarize_pyrep_shape(&args[2])
            ))
        }
        _ => unreachable!("unhandled deferred compat event '{event}'"),
    }
}

fn check_incomplete_allowlist_top_level(
    event: &str,
    samples: &[Vec<PyRep>],
    args: &[PyRep],
) -> Result<(), String> {
    if event == "OnSharedBookmarksFolderUpdated" {
        return check_shared_bookmarks_folder_updated(samples, args);
    }
    let same_len: Vec<(usize, &Vec<PyRep>)> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.len() == args.len())
        .collect();
    if !same_len.is_empty() {
        let mut failures = Vec::new();
        for (sample_idx, sample) in same_len {
            let mut mismatch = None;
            for (i, (a, s)) in args.iter().zip(sample.iter()).enumerate() {
                if discriminant_name(a) != discriminant_name(s) {
                    mismatch = Some(format!(
                        "sample[{sample_idx}] failed at arg[{i}]: expected top-level {}, got {}",
                        discriminant_name(s),
                        summarize_pyrep_shape(a)
                    ));
                    break;
                }
            }
            match mismatch {
                None => return Ok(()),
                Some(msg) => failures.push(msg),
            }
        }
        return Err(format!(
            "Notification top-level shape mismatch for allowlisted event '{event}': {}; actual={}; expected sample(s)={}",
            failures.join("; "),
            summarize_args_shape(args),
            summarize_samples_list(samples)
        ));
    }

    let base_sample_len = samples.iter().map(Vec::len).min().unwrap_or(0);
    for (i, actual_arg) in args.iter().enumerate() {
        let mut expected_discriminants: Vec<&'static str> = samples
            .iter()
            .filter_map(|s| s.get(i).map(discriminant_name))
            .collect();
        if expected_discriminants.is_empty() {
            if let Some(allowed_trailing) = INCOMPLETE_TRAILING_SLOT_DISCRIMINANTS
                .iter()
                .find(|(e, _)| *e == event)
                .and_then(|(_, slots)| i.checked_sub(base_sample_len).and_then(|off| slots.get(off)))
            {
                expected_discriminants.extend_from_slice(allowed_trailing);
            }
        }
        if expected_discriminants.is_empty() {
            return Err(format!(
                "Notification top-level shape mismatch for allowlisted event '{event}' at arg[{i}]: missing sample or trailing discriminant rule for arg[{i}], got {}; actual={}; expected sample(s)={}",
                summarize_pyrep_shape(actual_arg),
                summarize_args_shape(args),
                summarize_samples_list(samples)
            ));
        }
        if !expected_discriminants.contains(&discriminant_name(actual_arg)) {
            return Err(format!(
                "Notification top-level shape mismatch for allowlisted event '{event}' at arg[{i}]: expected one of {:?}, got {}; actual={}; expected sample(s)={}",
                expected_discriminants,
                summarize_pyrep_shape(actual_arg),
                summarize_args_shape(args),
                summarize_samples_list(samples)
            ));
        }
    }
    Ok(())
}

fn try_match_with_keyval_nullable_unions(
    event: &str,
    same_len: &[(usize, &Vec<PyRep>)],
    args: &[PyRep],
) -> bool {
    if same_len.len() < 2 || !args.iter().any(|a| matches!(a, PyRep::KeyVal(_))) {
        return false;
    }
    let kv_slots: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(idx, a)| matches!(a, PyRep::KeyVal(_)).then_some(idx))
        .collect();
    let cohort: Vec<&Vec<PyRep>> = same_len
        .iter()
        .filter_map(|(_, s)| {
            let ok = args.iter().enumerate().all(|(idx, a)| match a {
                PyRep::KeyVal(akv) => matches!(
                    &s[idx],
                    PyRep::KeyVal(skv)
                        if skv.len() == akv.len()
                            && akv.keys().all(|k| skv.contains_key(k.as_str()))
                ),
                _ => match_pyrep_recursive(event, a, &s[idx], &format!("arg[{idx}]"), 0).is_ok(),
            });
            ok.then_some(*s)
        })
        .collect();
    if cohort.is_empty() {
        return false;
    }
    kv_slots.into_iter().all(|slot_idx| {
        let PyRep::KeyVal(akv) = &args[slot_idx] else {
            return false;
        };
        akv.iter().all(|(k, av)| {
            let field_path = format!("arg[{slot_idx}].{k}");
            cohort.iter().any(|s| {
                let PyRep::KeyVal(skv) = &s[slot_idx] else {
                    return false;
                };
                skv.get(k.as_str()).is_some_and(|sv| {
                    match_pyrep_recursive(event, av, sv, &field_path, 1).is_ok()
                })
            })
        })
    })
}

/// Validates an emitted `args` slice against registered arity bounds and sample payload shapes.
pub fn check_event_shape_against_samples(
    event: &str,
    min_arity: usize,
    max_arity: usize,
    samples: &[Vec<PyRep>],
    args: &[PyRep],
) -> Result<(), String> {
    if args.len() < min_arity || args.len() > max_arity {
        return Err(format!(
            "Notification shape mismatch for event '{event}': arity mismatch at arg[{}] (got {} args, registered arity {min_arity}..={max_arity}); actual={}; expected sample(s)={}",
            args.len(),
            args.len(),
            summarize_args_shape(args),
            summarize_samples_list(samples)
        ));
    }
    if samples.is_empty() {
        return Err(format!(
            "Notification shape check failed for event '{event}': sample_payloads() is empty; actual={}",
            summarize_args_shape(args)
        ));
    }
    if is_incomplete_sample_event(event) {
        return check_incomplete_allowlist_top_level(event, samples, args);
    }
    if is_deferred_6b2_compat_event(event) {
        return check_deferred_6b2_compat(event, samples, args);
    }

    let same_len: Vec<(usize, &Vec<PyRep>)> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.len() == args.len())
        .collect();
    if same_len.is_empty() {
        let lens: Vec<String> = samples.iter().map(|s| s.len().to_string()).collect();
        return Err(format!(
            "Notification shape mismatch for event '{event}': arity {} at arg[{}] has no matching sample length (sample lengths: [{}]); actual={}; expected sample(s)={}",
            args.len(),
            args.len(),
            lens.join(", "),
            summarize_args_shape(args),
            summarize_samples_list(samples)
        ));
    }

    let mut failures = Vec::new();
    for &(sample_idx, sample) in &same_len {
        let mut mismatch = None;
        for (i, (actual_arg, sample_arg)) in args.iter().zip(sample.iter()).enumerate() {
            if let Err(err) =
                match_pyrep_recursive(event, actual_arg, sample_arg, &format!("arg[{i}]"), 0)
            {
                mismatch = Some(format!("sample[{sample_idx}] failed at {err}"));
                break;
            }
        }
        match mismatch {
            None => return Ok(()),
            Some(msg) => failures.push(msg),
        }
    }

    if try_match_with_keyval_nullable_unions(event, &same_len, args) {
        return Ok(());
    }

    Err(format!(
        "Notification shape mismatch for event '{event}': {}; actual={}; expected sample(s)={}",
        failures.join("; "),
        summarize_args_shape(args),
        summarize_samples_list(samples)
    ))
}

/// Validates an emitted `args` slice against a registered [`NotificationRegistryEntry`].
pub fn check_notification_shape(
    entry: &NotificationRegistryEntry,
    args: &[PyRep],
) -> Result<(), String> {
    let samples = (entry.sample_payloads)();
    check_event_shape_against_samples(entry.event, entry.min_arity, entry.max_arity, &samples, args)
}

/// Panics with a detailed diagnostic if `args` does not match `entry`'s registered shape(s).
#[track_caller]
pub fn assert_registered_notification_shape(entry: &NotificationRegistryEntry, args: &[PyRep]) {
    if let Err(msg) = check_notification_shape(entry, args) {
        panic!("{msg}");
    }
}

/// Panics with a detailed diagnostic if `args` for typed notification `N` fails shape validation.
///
/// Uses the canonical entry in [`all_registered_notifications`] whenever `N::EVENT` is registered,
/// falling back to `N::sample_payloads()` only for legacy/internal events outside the registry.
#[track_caller]
pub fn assert_typed_notification_shape<N: TypedNotification>(args: &[PyRep]) {
    if let Some(entry) = all_registered_notifications()
        .iter()
        .find(|e| e.event == N::EVENT)
    {
        assert_registered_notification_shape(entry, args);
    } else {
        let samples = N::sample_payloads();
        if let Err(msg) =
            check_event_shape_against_samples(N::EVENT, N::MIN_ARITY, N::MAX_ARITY, &samples, args)
        {
            panic!("{msg}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_proto::DBRowDescriptor;

    #[test]
    fn shape_check_matches_exact_and_multi_sample_shapes() {
        let samples = vec![
            vec![
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Dict(vec![
                    (
                        PyRep::String("stationid".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_760)]),
                    ),
                    (
                        PyRep::String("solarsystemid2".into()),
                        PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(30_000_144)]),
                    ),
                ]),
            ],
            vec![
                PyRep::Bool(true),
                PyRep::keyval([("charid", PyRep::Integer(90_000_001))]),
                PyRep::Dict(vec![(
                    PyRep::String("stationid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_760)]),
                )]),
            ],
        ];

        assert!(check_event_shape_against_samples(
            "TestMulti",
            3,
            3,
            &samples,
            &[
                PyRep::Bool(true),
                PyRep::None,
                PyRep::Dict(vec![(
                    PyRep::String("stationid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_761)]),
                )]),
            ],
        )
        .is_ok());

        assert!(check_event_shape_against_samples(
            "TestMulti",
            3,
            3,
            &samples,
            &[
                PyRep::Bool(false),
                PyRep::keyval([("charid", PyRep::Integer(90_000_002))]),
                PyRep::Dict(Vec::new()),
            ],
        )
        .is_ok());

        let err_extra = check_event_shape_against_samples(
            "TestMulti",
            3,
            3,
            &samples,
            &[
                PyRep::Bool(false),
                PyRep::keyval([
                    ("charid", PyRep::Integer(90_000_002)),
                    ("extraField", PyRep::String("ok".into())),
                ]),
                PyRep::Dict(Vec::new()),
            ],
        )
        .unwrap_err();
        assert!(err_extra.contains("extraField"), "{err_extra}");
    }

    #[test]
    fn shape_check_rejects_scalar_and_none_mismatches() {
        let int_sample = vec![vec![PyRep::Integer(1)]];
        let err_kv = check_event_shape_against_samples(
            "TestScalar",
            1,
            1,
            &int_sample,
            &[PyRep::keyval([("id", PyRep::Integer(1))])],
        )
        .unwrap_err();
        assert!(err_kv.contains("TestScalar") && err_kv.contains("arg[0]"), "{err_kv}");

        let err_str = check_event_shape_against_samples(
            "TestScalar",
            1,
            1,
            &int_sample,
            &[PyRep::String("1".into())],
        )
        .unwrap_err();
        assert!(err_str.contains("TestScalar") && err_str.contains("arg[0]"), "{err_str}");

        let err_none = check_event_shape_against_samples(
            "TestScalar",
            1,
            1,
            &int_sample,
            &[PyRep::None],
        )
        .unwrap_err();
        assert!(err_none.contains("TestScalar") && err_none.contains("arg[0]"), "{err_none}");
    }

    #[test]
    fn shape_check_rejects_keyval_missing_and_wrong_field_types() {
        let samples = vec![vec![PyRep::keyval([
            ("itemID", PyRep::Integer(1)),
            ("name", PyRep::String("Rifter".into())),
        ])]];

        let err_missing = check_event_shape_against_samples(
            "TestKeyVal",
            1,
            1,
            &samples,
            &[PyRep::keyval([("itemID", PyRep::Integer(2))])],
        )
        .unwrap_err();
        assert!(
            err_missing.contains("TestKeyVal") && err_missing.contains("arg[0].name"),
            "{err_missing}"
        );

        let err_type = check_event_shape_against_samples(
            "TestKeyVal",
            1,
            1,
            &samples,
            &[PyRep::keyval([
                ("itemID", PyRep::Integer(2)),
                ("name", PyRep::Integer(99)),
            ])],
        )
        .unwrap_err();
        assert!(
            err_type.contains("TestKeyVal") && err_type.contains("arg[0].name"),
            "{err_type}"
        );

        let err_dict_for_kv = check_event_shape_against_samples(
            "TestKeyVal",
            1,
            1,
            &samples,
            &[PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(2)),
                (PyRep::String("name".into()), PyRep::String("Rifter".into())),
            ])],
        )
        .unwrap_err();
        assert!(err_dict_for_kv.contains("arg[0]"), "{err_dict_for_kv}");
    }

    #[test]
    fn shape_check_rejects_tuple_list_dict_and_arity_mismatches() {
        // Tuple length & element mismatch
        let tuple_samples = vec![vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(2.0)])]];
        assert!(check_event_shape_against_samples(
            "TestTuple",
            1,
            1,
            &tuple_samples,
            &[PyRep::Tuple(vec![PyRep::Integer(1)])],
        )
        .is_err());
        assert!(check_event_shape_against_samples(
            "TestTuple",
            1,
            1,
            &tuple_samples,
            &[PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)])],
        )
        .is_err());

        // List element mismatch against non-empty and empty samples
        let list_nonempty = vec![vec![PyRep::List(vec![PyRep::Integer(1)])]];
        assert!(check_event_shape_against_samples(
            "TestList",
            1,
            1,
            &list_nonempty,
            &[PyRep::List(Vec::new())],
        )
        .is_ok());
        assert!(check_event_shape_against_samples(
            "TestList",
            1,
            1,
            &list_nonempty,
            &[PyRep::List(vec![PyRep::String("bad".into())])],
        )
        .is_err());
        let list_empty = vec![vec![PyRep::List(Vec::new())]];
        assert!(check_event_shape_against_samples(
            "TestListEmpty",
            1,
            1,
            &list_empty,
            &[PyRep::List(vec![PyRep::Integer(1)])],
        )
        .is_err());

        // Dict element mismatch against non-empty (including fixed string key) and empty samples
        let dict_nonempty = vec![vec![PyRep::Dict(vec![
            (PyRep::String("count".into()), PyRep::Integer(1)),
            (PyRep::String("label".into()), PyRep::String("ok".into())),
        ])]];
        assert!(check_event_shape_against_samples(
            "TestDict",
            1,
            1,
            &dict_nonempty,
            &[PyRep::Dict(Vec::new())],
        )
        .is_ok());
        assert!(check_event_shape_against_samples(
            "TestDict",
            1,
            1,
            &dict_nonempty,
            &[PyRep::Dict(vec![(
                PyRep::String("count".into()),
                PyRep::String("wrong".into()),
            )])],
        )
        .is_err());
        let dict_empty = vec![vec![PyRep::Dict(Vec::new())]];
        assert!(check_event_shape_against_samples(
            "TestDictEmpty",
            1,
            1,
            &dict_empty,
            &[PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(2))])],
        )
        .is_err());

        // Arity mismatch
        let err_arity = check_event_shape_against_samples(
            "TestArity",
            1,
            2,
            &[vec![PyRep::Integer(1)]],
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)],
        )
        .unwrap_err();
        assert!(err_arity.contains("TestArity") && err_arity.contains("arity"), "{err_arity}");
    }

    #[test]
    fn shape_check_validates_structured_wire_reps() {
        let rowset_sample = vec![vec![PyRep::rowset(
            vec!["itemID", "name"],
            vec![vec![PyRep::Integer(1), PyRep::String("A".into())]],
        )]];
        assert!(check_event_shape_against_samples(
            "TestRowset",
            1,
            1,
            &rowset_sample,
            &[PyRep::rowset(vec!["itemID", "name"], Vec::new())],
        )
        .is_ok());
        assert!(check_event_shape_against_samples(
            "TestRowset",
            1,
            1,
            &rowset_sample,
            &[PyRep::rowset(
                vec!["itemID", "name"],
                vec![vec![PyRep::Integer(1), PyRep::Integer(2)]],
            )],
        )
        .is_err());

        let dbrow = DBRowDescriptor::inv_item_header().build_dbrow(vec![
            PyRep::Integer(1),
            PyRep::Integer(34),
            PyRep::Integer(2),
            PyRep::Integer(3),
            PyRep::Integer(4),
            PyRep::Integer(1),
            PyRep::Integer(18),
            PyRep::Integer(4),
            PyRep::String(String::new()),
        ]);
        let wrapped_sample = vec![vec![PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(
            dbrow.clone(),
        ))))]];
        assert!(check_event_shape_against_samples(
            "TestWrapped",
            1,
            1,
            &wrapped_sample,
            &[PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(dbrow))))],
        )
        .is_ok());
        assert!(check_event_shape_against_samples(
            "TestWrapped",
            1,
            1,
            &wrapped_sample,
            &[PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(
                PyRep::Integer(1),
            ))))],
        )
        .is_err());
    }

    #[test]
    fn incomplete_allowlist_validates_trailing_slots_strictly() {
        let trade_offer_samples = vec![vec![PyRep::Integer(1), PyRep::Bool(true)]];
        assert!(check_event_shape_against_samples(
            "OnTradeOffer",
            2,
            3,
            &trade_offer_samples,
            &[PyRep::Integer(1), PyRep::Bool(false), PyRep::Bool(true)],
        )
        .is_ok());
        let err_offer = check_event_shape_against_samples(
            "OnTradeOffer",
            2,
            3,
            &trade_offer_samples,
            &[PyRep::Integer(1), PyRep::Bool(false), PyRep::Integer(999)],
        )
        .unwrap_err();
        assert!(
            err_offer.contains("OnTradeOffer") && err_offer.contains("arg[2]"),
            "{err_offer}"
        );

        let money_offer_samples = vec![vec![PyRep::Integer(1), PyRep::Real(5_000.0)]];
        assert!(check_event_shape_against_samples(
            "OnTradeMoneyOffer",
            2,
            3,
            &money_offer_samples,
            &[PyRep::Integer(1), PyRep::Real(0.0), PyRep::Real(5_000.0)],
        )
        .is_ok());
        let err_money = check_event_shape_against_samples(
            "OnTradeMoneyOffer",
            2,
            3,
            &money_offer_samples,
            &[
                PyRep::Integer(1),
                PyRep::Real(0.0),
                PyRep::String("bad".into()),
            ],
        )
        .unwrap_err();
        assert!(
            err_money.contains("OnTradeMoneyOffer") && err_money.contains("arg[2]"),
            "{err_money}"
        );
    }

    #[test]
    fn shape_check_rejects_misspelled_fixed_dict_keys_and_allows_dynamic_string_maps() {
        let on_session_changed = all_registered_notifications()
            .into_iter()
            .find(|e| e.event == "OnSessionChanged")
            .expect("OnSessionChanged entry");
        let err_session = check_notification_shape(
            on_session_changed,
            &[
                PyRep::Bool(false),
                PyRep::None,
                PyRep::Dict(vec![(
                    PyRep::String("solarSystemId".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(30_000_142)]),
                )]),
            ],
        )
        .unwrap_err();
        assert!(
            err_session.contains("solarSystemId") && err_session.contains("OnSessionChanged"),
            "{err_session}"
        );

        let on_combat_msg = all_registered_notifications()
            .into_iter()
            .find(|e| e.event == "OnCombatMessage")
            .expect("OnCombatMessage entry");
        let err_combat = check_notification_shape(
            on_combat_msg,
            &[
                PyRep::String("Hit".into()),
                PyRep::Dict(vec![(PyRep::String("damaeg".into()), PyRep::Real(12.5))]),
            ],
        )
        .unwrap_err();
        assert!(
            err_combat.contains("damaeg") && err_combat.contains("OnCombatMessage"),
            "{err_combat}"
        );

        let on_anom = all_registered_notifications()
            .into_iter()
            .find(|e| e.event == "OnSignalTrackerAnomalyUpdate")
            .expect("OnSignalTrackerAnomalyUpdate entry");
        assert!(check_notification_shape(
            on_anom,
            &[
                PyRep::Integer(30_000_142),
                PyRep::Dict(vec![(
                    PyRep::String("ANOM-DYNAMIC-999".into()),
                    PyRep::keyval([
                        ("instanceID", PyRep::String("ANOM-DYNAMIC-999".into())),
                        (
                            "position",
                            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                        ),
                        ("targetID", PyRep::String("ANOM-DYNAMIC-999".into())),
                        ("difficulty", PyRep::Integer(1)),
                        ("dungeonID", PyRep::Integer(2_134)),
                        ("archetypeID", PyRep::Integer(24)),
                        ("dungeonName", PyRep::String("Dynamic".into())),
                        ("dungeonNameID", PyRep::None),
                        ("factionID", PyRep::Integer(500_010)),
                        ("scanStrengthAttribute", PyRep::Integer(210)),
                        ("allowedTypes", PyRep::None),
                    ]),
                )]),
                PyRep::List(Vec::new()),
            ],
        )
        .is_ok());
    }

    #[test]
    fn shape_check_rejects_unexpected_extra_keyval_fields() {
        let samples = vec![vec![PyRep::keyval([
            ("itemID", PyRep::Integer(1)),
            ("name", PyRep::String("Rifter".into())),
        ])]];
        let err = check_event_shape_against_samples(
            "TestClosedKeyVal",
            1,
            1,
            &samples,
            &[PyRep::keyval([
                ("itemID", PyRep::Integer(2)),
                ("name", PyRep::String("Rifter".into())),
                ("unexpectedField", PyRep::Bool(true)),
            ])],
        )
        .unwrap_err();
        assert!(
            err.contains("TestClosedKeyVal") && err.contains("unexpectedField"),
            "{err}"
        );
    }

    #[test]
    fn shape_check_shared_bookmarks_validates_nested_tuples_strictly() {
        let entry = all_registered_notifications()
            .into_iter()
            .find(|e| e.event == "OnSharedBookmarksFolderUpdated")
            .expect("OnSharedBookmarksFolderUpdated entry");

        let err_garbage =
            check_notification_shape(entry, &[PyRep::List(vec![PyRep::String("garbage".into())])])
                .unwrap_err();
        assert!(
            err_garbage.contains("OnSharedBookmarksFolderUpdated")
                && err_garbage.contains("arg[0][0]"),
            "{err_garbage}"
        );

        let err_bad_key = check_notification_shape(
            entry,
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("not-tuple".into()),
                PyRep::String("FolderChanged".into()),
                PyRep::Integer(1),
            ])])],
        )
        .unwrap_err();
        assert!(err_bad_key.contains("arg[0][0][0]"), "{err_bad_key}");

        assert!(check_notification_shape(
            entry,
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(7)]),
                PyRep::String("FolderChanged".into()),
                PyRep::Integer(90_000_001),
            ])])],
        )
        .is_ok());

        assert!(check_notification_shape(
            entry,
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(7)]),
                PyRep::String("FolderChanged".into()),
                PyRep::Tuple(vec![
                    PyRep::List(vec![PyRep::keyval([("bookmarkID", PyRep::Integer(55))])]),
                    PyRep::List(vec![PyRep::Integer(55)]),
                ]),
            ])])],
        )
        .is_ok());
    }

    #[test]
    fn shape_check_closed_keyval_nullable_unions_reject_cross_anchor_and_extra_keys() {
        let samples = vec![
            vec![
                PyRep::Integer(1),
                PyRep::keyval([
                    ("a", PyRep::Integer(10)),
                    ("b", PyRep::Integer(20)),
                ]),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::keyval([
                    ("a", PyRep::None),
                    ("b", PyRep::None),
                ]),
            ],
            vec![
                PyRep::String("str-anchor".into()),
                PyRep::keyval([
                    ("a", PyRep::String("x".into())),
                    ("b", PyRep::Integer(30)),
                ]),
            ],
        ];
        assert!(check_event_shape_against_samples(
            "TestUnionKeyVal",
            2,
            2,
            &samples,
            &[
                PyRep::Integer(2),
                PyRep::keyval([("a", PyRep::Integer(1)), ("b", PyRep::None)]),
            ],
        )
        .is_ok());
        // Cross-anchor mixing (`a: String` from sample[2] with arg[0]=Integer) must fail.
        assert!(check_event_shape_against_samples(
            "TestUnionKeyVal",
            2,
            2,
            &samples,
            &[
                PyRep::Integer(2),
                PyRep::keyval([("a", PyRep::String("x".into())), ("b", PyRep::None)]),
            ],
        )
        .is_err());
        // Extra or missing keys must still fail cleanly.
        assert!(check_event_shape_against_samples(
            "TestUnionKeyVal",
            2,
            2,
            &samples,
            &[
                PyRep::Integer(2),
                PyRep::keyval([
                    ("a", PyRep::None),
                    ("b", PyRep::None),
                    ("c", PyRep::Integer(1)),
                ]),
            ],
        )
        .is_err());
    }
}
