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

/// Returns `true` if `event` is on the shrink-only incomplete-sample allowlist.
pub fn is_incomplete_sample_event(event: &str) -> bool {
    INCOMPLETE_SAMPLE_EVENTS.contains(&event)
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
            .any(|s_item| match_pyrep_recursive(item, s_item, &item_path, depth + 1).is_ok())
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
                    .any(|sv| match_pyrep_recursive(av, sv, &val_path, depth + 1).is_ok())
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
        }
        let key_path = format!("{path}[key#{idx}]");
        let val_path = format!("{path}[val#{idx}]");
        if !sample.iter().any(|(sk, sv)| {
            match_pyrep_recursive(ak, sk, &key_path, depth + 1).is_ok()
                && match_pyrep_recursive(av, sv, &val_path, depth + 1).is_ok()
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
        match_pyrep_recursive(av, sv, &field_path, depth + 1)?;
    }
    Ok(())
}

fn match_row_against_samples(
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
        match_row_against_samples(kind, r_idx, row, ac.len(), sr, path, depth)?;
    }
    Ok(())
}

fn match_pyrep_recursive(
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
                match_pyrep_recursive(ae, se, &format!("{path}[{idx}]"), depth + 1)?;
            }
            Ok(())
        }
        (PyRep::List(a), PyRep::List(s)) => {
            match_collection_elements("List", a, s, path, depth)
        }
        (PyRep::Set(a), PyRep::Set(s)) => {
            match_collection_elements("Set", a, s, path, depth)
        }
        (PyRep::PySet(a), PyRep::PySet(s)) => {
            match_collection_elements("PySet", a, s, path, depth)
        }
        (PyRep::Dict(a), PyRep::Dict(s)) => match_dict_entries(a, s, path, depth),
        (PyRep::KeyVal(a), PyRep::KeyVal(s)) => match_keyval_fields(a, s, path, depth),
        (
            PyRep::Rowset {
                columns: ac,
                rows: ar,
            },
            PyRep::Rowset {
                columns: sc,
                rows: sr,
            },
        ) => match_tabular_rep("Rowset", ac, ar, sc, sr, path, depth),
        (
            PyRep::TupleSet {
                columns: ac,
                rows: ar,
            },
            PyRep::TupleSet {
                columns: sc,
                rows: sr,
            },
        ) => match_tabular_rep("TupleSet", ac, ar, sc, sr, path, depth),
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
                        && match_pyrep_recursive(k, sk_i, &format!("{path}.items[{idx}].key"), depth + 1)
                            .is_ok()
                        && row.iter().enumerate().all(|(c_idx, cell)| {
                            match_pyrep_recursive(
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
            match_pyrep_recursive(ah, sh, &format!("{path}.header"), depth + 1)?;
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
                match_pyrep_recursive(av, sv, &format!("{path}.{an}"), depth + 1)?;
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
            match_pyrep_recursive(aa, sa, &format!("{path}<{at}>"), depth + 1)
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
            match_pyrep_recursive(ah, sh, &format!("{path}.header"), depth + 1)?;
            match_collection_elements("ObjectEx.list", al, sl, &format!("{path}.list"), depth)?;
            match_dict_entries(ad, sd, &format!("{path}.dict"), depth)
        }
        (PyRep::SubStruct(a), PyRep::SubStruct(s)) => {
            match_pyrep_recursive(a, s, &format!("{path}.SubStruct"), depth + 1)
        }
        (PyRep::ChecksumedStream { body: a, .. }, PyRep::ChecksumedStream { body: s, .. }) => {
            match_pyrep_recursive(a, s, &format!("{path}.ChecksumedStream"), depth + 1)
        }
        (PyRep::SubStream(a), PyRep::SubStream(s)) => {
            match_pyrep_recursive(a, s, &format!("{path}.SubStream"), depth + 1)
        }
        (a, s) => Err(format!(
            "{path}: expected {}, got {}",
            summarize_pyrep_shape(s),
            summarize_pyrep_shape(a)
        )),
    }
}

fn check_incomplete_allowlist_top_level(
    event: &str,
    samples: &[Vec<PyRep>],
    args: &[PyRep],
) -> Result<(), String> {
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

    for (i, actual_arg) in args.iter().enumerate() {
        let expected_discriminants: Vec<&'static str> = samples
            .iter()
            .filter_map(|s| s.get(i).map(discriminant_name))
            .collect();
        if !expected_discriminants.is_empty()
            && !expected_discriminants.contains(&discriminant_name(actual_arg))
        {
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
    for (sample_idx, sample) in same_len {
        let mut mismatch = None;
        for (i, (actual_arg, sample_arg)) in args.iter().zip(sample.iter()).enumerate() {
            if let Err(err) = match_pyrep_recursive(actual_arg, sample_arg, &format!("arg[{i}]"), 0) {
                mismatch = Some(format!("sample[{sample_idx}] failed at {err}"));
                break;
            }
        }
        match mismatch {
            None => return Ok(()),
            Some(msg) => failures.push(msg),
        }
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
                PyRep::keyval([
                    ("charid", PyRep::Integer(90_000_002)),
                    ("extraField", PyRep::String("ok".into())),
                ]),
                PyRep::Dict(Vec::new()),
            ],
        )
        .is_ok());
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
}
