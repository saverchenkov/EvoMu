//! Typed, checked RPC argument extractor (`#594` / `[Arch-12]`).
//!
//! Replaces unchecked `args.first()...unwrap_or(0)` / `args.get(N)...unwrap_or(false)` patterns
//! with explicit `req_*` and `opt_*` extractors that check `kwargs` fallback, reject `NaN`/`Infinity`,
//! avoid silent truthiness bugs on booleans, and return structured `BridgeRpcError` (`InvalidArgument`).

#![forbid(unsafe_code)]

use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::OnceLock;

static EMPTY_KWARGS: OnceLock<HashMap<String, PyRep>> = OnceLock::new();

fn empty_kwargs() -> &'static HashMap<String, PyRep> {
    EMPTY_KWARGS.get_or_init(HashMap::new)
}

fn invalid_arg(message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: "InvalidArgument".into(),
        message: message.into(),
    }
}

fn arg_label(idx: usize, kw_name: &str) -> String {
    if kw_name.is_empty() {
        format!("arg[{idx}]")
    } else {
        format!("'{kw_name}' (arg[{idx}])")
    }
}

fn parse_strict_i64(rep: &PyRep, label: &str) -> Result<i64, BridgeRpcError> {
    match rep {
        PyRep::Integer(v) => Ok(*v),
        _ => Err(invalid_arg(format!("Expected integer for {label}"))),
    }
}

fn parse_strict_finite_f64(rep: &PyRep, label: &str) -> Result<f64, BridgeRpcError> {
    let val = match rep {
        PyRep::Real(v) => *v,
        PyRep::Integer(v) => *v as f64,
        _ => return Err(invalid_arg(format!("Expected finite number for {label}"))),
    };
    if !val.is_finite() {
        return Err(invalid_arg(format!(
            "Non-finite floating-point value for {label}"
        )));
    }
    Ok(val)
}

fn parse_strict_bool(rep: &PyRep, label: &str) -> Result<bool, BridgeRpcError> {
    match rep {
        PyRep::Bool(b) => Ok(*b),
        PyRep::Integer(0) => Ok(false),
        PyRep::Integer(1) => Ok(true),
        _ => Err(invalid_arg(format!("Expected boolean for {label}"))),
    }
}

fn parse_boolish(rep: &PyRep, label: &str) -> Result<bool, BridgeRpcError> {
    match rep {
        PyRep::Bool(b) => Ok(*b),
        PyRep::Integer(0) => Ok(false),
        PyRep::Integer(1) => Ok(true),
        _ => Err(invalid_arg(format!("Expected boolean or 0/1 for {label}"))),
    }
}

fn parse_strict_str<'a>(rep: &'a PyRep, label: &str) -> Result<&'a str, BridgeRpcError> {
    rep.as_str()
        .ok_or_else(|| invalid_arg(format!("Expected string for {label}")))
}

/// Borrowed view over MachoNet positional `args` and keyword `kwargs` with checked extractors.
#[derive(Debug, Clone, Copy)]
pub struct RpcArgs<'a> {
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
}

impl<'a> RpcArgs<'a> {
    /// Creates an extractor over positional `args` and keyword `kwargs`.
    pub fn new(args: &'a [PyRep], kwargs: &'a HashMap<String, PyRep>) -> Self {
        Self { args, kwargs }
    }

    /// Creates an extractor over positional `args` with empty `kwargs`.
    pub fn positional(args: &'a [PyRep]) -> Self {
        Self {
            args,
            kwargs: empty_kwargs(),
        }
    }

    pub fn args(&self) -> &'a [PyRep] {
        self.args
    }

    pub fn kwargs(&self) -> &'a HashMap<String, PyRep> {
        self.kwargs
    }

    pub fn len(&self) -> usize {
        self.args.len()
    }

    pub fn is_empty(&self) -> bool {
        self.args.is_empty() && self.kwargs.is_empty()
    }

    /// Looks up a raw non-`None` argument at positional `idx`, falling back to `kwargs[kw_name]`.
    pub fn get_raw(&self, idx: usize, kw_name: &str) -> Option<&'a PyRep> {
        if let Some(v) = self.args.get(idx) {
            if !matches!(v, PyRep::None) {
                return Some(v);
            }
        }
        if !kw_name.is_empty() {
            if let Some(v) = self.kwargs.get(kw_name) {
                if !matches!(v, PyRep::None) {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Looks up a raw non-`None` argument at positional `idx`, falling back to any matching `kwargs` key.
    pub fn get_raw_any(&self, idx: usize, kw_names: &[&str]) -> Option<&'a PyRep> {
        if let Some(v) = self.args.get(idx) {
            if !matches!(v, PyRep::None) {
                return Some(v);
            }
        }
        for &kw in kw_names {
            if !kw.is_empty() {
                if let Some(v) = self.kwargs.get(kw) {
                    if !matches!(v, PyRep::None) {
                        return Some(v);
                    }
                }
            }
        }
        None
    }

    /// Extracts a required `i64` from `args[idx]` or `kwargs[kw_name]`.
    pub fn req_i64(&self, idx: usize, kw_name: &str) -> Result<i64, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required integer {label}")))?;
        parse_strict_i64(rep, &label)
    }

    /// Extracts an optional `i64`; returns `Ok(None)` if absent/`None`, or `Err` if malformed.
    pub fn opt_i64(&self, idx: usize, kw_name: &str) -> Result<Option<i64>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(None);
        };
        parse_strict_i64(rep, &arg_label(idx, kw_name)).map(Some)
    }

    /// Extracts a required `i64` checking `args[idx]` or any alias in `kw_names`.
    pub fn req_i64_any(&self, idx: usize, kw_names: &[&str]) -> Result<i64, BridgeRpcError> {
        let primary = kw_names.first().copied().unwrap_or("");
        let label = arg_label(idx, primary);
        let rep = self
            .get_raw_any(idx, kw_names)
            .ok_or_else(|| invalid_arg(format!("Missing required integer {label}")))?;
        parse_strict_i64(rep, &label)
    }

    /// Extracts an optional `i64` checking `args[idx]` or any alias in `kw_names`.
    pub fn opt_i64_any(
        &self,
        idx: usize,
        kw_names: &[&str],
    ) -> Result<Option<i64>, BridgeRpcError> {
        let primary = kw_names.first().copied().unwrap_or("");
        let Some(rep) = self.get_raw_any(idx, kw_names) else {
            return Ok(None);
        };
        parse_strict_i64(rep, &arg_label(idx, primary)).map(Some)
    }

    /// Extracts a required strictly positive (`> 0`) `i64`.
    pub fn req_positive_i64(&self, idx: usize, kw_name: &str) -> Result<i64, BridgeRpcError> {
        let val = self.req_i64(idx, kw_name)?;
        if val <= 0 {
            return Err(invalid_arg(format!(
                "{} must be positive (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(val)
    }

    /// Extracts an optional strictly positive (`> 0`) `i64`.
    pub fn opt_positive_i64(
        &self,
        idx: usize,
        kw_name: &str,
    ) -> Result<Option<i64>, BridgeRpcError> {
        let Some(val) = self.opt_i64(idx, kw_name)? else {
            return Ok(None);
        };
        if val <= 0 {
            return Err(invalid_arg(format!(
                "{} must be positive (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(Some(val))
    }

    /// Extracts a required non-negative (`>= 0`) `i64`.
    pub fn req_non_negative_i64(&self, idx: usize, kw_name: &str) -> Result<i64, BridgeRpcError> {
        let val = self.req_i64(idx, kw_name)?;
        if val < 0 {
            return Err(invalid_arg(format!(
                "{} must be non-negative (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(val)
    }

    /// Extracts a required `u64` (rejects negative values).
    pub fn req_u64(&self, idx: usize, kw_name: &str) -> Result<u64, BridgeRpcError> {
        let val = self.req_non_negative_i64(idx, kw_name)?;
        Ok(val as u64)
    }

    /// Extracts an optional `u64` (rejects negative or malformed values).
    pub fn opt_u64(&self, idx: usize, kw_name: &str) -> Result<Option<u64>, BridgeRpcError> {
        let Some(val) = self.opt_i64(idx, kw_name)? else {
            return Ok(None);
        };
        if val < 0 {
            return Err(invalid_arg(format!(
                "{} must be non-negative (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(Some(val as u64))
    }

    /// Extracts a required `u32` (rejects `< 0` or `> u32::MAX`).
    pub fn req_u32(&self, idx: usize, kw_name: &str) -> Result<u32, BridgeRpcError> {
        let val = self.req_i64(idx, kw_name)?;
        u32::try_from(val).map_err(|_| {
            invalid_arg(format!(
                "{} out of u32 range (got {val})",
                arg_label(idx, kw_name)
            ))
        })
    }

    /// Extracts an optional `u32` (rejects `< 0`, `> u32::MAX`, or malformed types).
    pub fn opt_u32(&self, idx: usize, kw_name: &str) -> Result<Option<u32>, BridgeRpcError> {
        let Some(val) = self.opt_i64(idx, kw_name)? else {
            return Ok(None);
        };
        u32::try_from(val)
            .map(Some)
            .map_err(|_| invalid_arg(format!("{} out of u32 range (got {val})", arg_label(idx, kw_name))))
    }

    /// Extracts a required `i32` (rejects out-of-range values).
    pub fn req_i32(&self, idx: usize, kw_name: &str) -> Result<i32, BridgeRpcError> {
        let val = self.req_i64(idx, kw_name)?;
        i32::try_from(val).map_err(|_| {
            invalid_arg(format!(
                "{} out of i32 range (got {val})",
                arg_label(idx, kw_name)
            ))
        })
    }

    /// Extracts an optional `i32` (rejects out-of-range or malformed values).
    pub fn opt_i32(&self, idx: usize, kw_name: &str) -> Result<Option<i32>, BridgeRpcError> {
        let Some(val) = self.opt_i64(idx, kw_name)? else {
            return Ok(None);
        };
        i32::try_from(val)
            .map(Some)
            .map_err(|_| invalid_arg(format!("{} out of i32 range (got {val})", arg_label(idx, kw_name))))
    }

    /// Extracts a required finite `f64` (accepts `Real` or `Integer`; rejects `NaN` / `±Infinity`).
    pub fn req_finite_f64(&self, idx: usize, kw_name: &str) -> Result<f64, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required finite float {label}")))?;
        parse_strict_finite_f64(rep, &label)
    }

    /// Extracts an optional finite `f64` (rejects `NaN` / `±Infinity` / malformed types).
    pub fn opt_finite_f64(&self, idx: usize, kw_name: &str) -> Result<Option<f64>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(None);
        };
        parse_strict_finite_f64(rep, &arg_label(idx, kw_name)).map(Some)
    }

    /// Extracts a required strictly positive finite `f64` (`> 0.0`, finite).
    pub fn req_positive_f64(&self, idx: usize, kw_name: &str) -> Result<f64, BridgeRpcError> {
        let val = self.req_finite_f64(idx, kw_name)?;
        if val <= 0.0 {
            return Err(invalid_arg(format!(
                "{} must be positive (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(val)
    }

    /// Extracts an optional strictly positive finite `f64` (`> 0.0`, finite).
    pub fn opt_positive_f64(
        &self,
        idx: usize,
        kw_name: &str,
    ) -> Result<Option<f64>, BridgeRpcError> {
        let Some(val) = self.opt_finite_f64(idx, kw_name)? else {
            return Ok(None);
        };
        if val <= 0.0 {
            return Err(invalid_arg(format!(
                "{} must be positive (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(Some(val))
    }

    /// Extracts a required non-negative finite `f64` (`>= 0.0`, finite).
    pub fn req_non_negative_f64(&self, idx: usize, kw_name: &str) -> Result<f64, BridgeRpcError> {
        let val = self.req_finite_f64(idx, kw_name)?;
        if val < 0.0 {
            return Err(invalid_arg(format!(
                "{} must be non-negative (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(val)
    }

    /// Extracts an optional non-negative finite `f64` (`>= 0.0`, finite).
    pub fn opt_non_negative_f64(
        &self,
        idx: usize,
        kw_name: &str,
    ) -> Result<Option<f64>, BridgeRpcError> {
        let Some(val) = self.opt_finite_f64(idx, kw_name)? else {
            return Ok(None);
        };
        if val < 0.0 {
            return Err(invalid_arg(format!(
                "{} must be non-negative (got {val})",
                arg_label(idx, kw_name)
            )));
        }
        Ok(Some(val))
    }

    /// Extracts a required finite 3D vector either from a 3-element tuple/list at `start_idx`
    /// or from 3 consecutive scalar args `(args[start_idx], args[start_idx + 1], args[start_idx + 2])`.
    /// Accepts `Integer` or `Real` coordinates and rejects `NaN`/`Infinity`/`-Infinity`.
    pub fn req_finite_vec3(&self, start_idx: usize) -> Result<DVec3, BridgeRpcError> {
        match self.args.get(start_idx) {
            Some(PyRep::Tuple(items)) | Some(PyRep::List(items)) => {
                if items.len() != 3 {
                    return Err(invalid_arg(format!(
                        "Expected 3 coordinates in vec3 at arg[{start_idx}], got {}",
                        items.len()
                    )));
                }
                let x = parse_strict_finite_f64(&items[0], &format!("arg[{start_idx}][0]"))?;
                let y = parse_strict_finite_f64(&items[1], &format!("arg[{start_idx}][1]"))?;
                let z = parse_strict_finite_f64(&items[2], &format!("arg[{start_idx}][2]"))?;
                Ok(DVec3::new(x, y, z))
            }
            _ => {
                let x_rep = self.args.get(start_idx).ok_or_else(|| {
                    invalid_arg(format!("Missing vec3 x coordinate at arg[{start_idx}]"))
                })?;
                let y_rep = self.args.get(start_idx + 1).ok_or_else(|| {
                    invalid_arg(format!(
                        "Missing vec3 y coordinate at arg[{}]",
                        start_idx + 1
                    ))
                })?;
                let z_rep = self.args.get(start_idx + 2).ok_or_else(|| {
                    invalid_arg(format!(
                        "Missing vec3 z coordinate at arg[{}]",
                        start_idx + 2
                    ))
                })?;
                let x = parse_strict_finite_f64(x_rep, &format!("arg[{start_idx}]"))?;
                let y = parse_strict_finite_f64(y_rep, &format!("arg[{}]", start_idx + 1))?;
                let z = parse_strict_finite_f64(z_rep, &format!("arg[{}]", start_idx + 2))?;
                Ok(DVec3::new(x, y, z))
            }
        }
    }

    /// Extracts an optional finite 3D vector starting at `start_idx`.
    /// Returns `Ok(None)` when `args.get(start_idx)` is absent or `PyRep::None`.
    pub fn opt_finite_vec3(&self, start_idx: usize) -> Result<Option<DVec3>, BridgeRpcError> {
        match self.args.get(start_idx) {
            None | Some(PyRep::None) => Ok(None),
            Some(_) => self.req_finite_vec3(start_idx).map(Some),
        }
    }

    /// Extracts a required strict boolean (`PyRep::Bool` only; rejects integers/strings).
    pub fn req_bool(&self, idx: usize, kw_name: &str) -> Result<bool, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required boolean {label}")))?;
        parse_strict_bool(rep, &label)
    }

    /// Extracts an optional strict boolean; returns `Ok(None)` if absent/`None`, or `Err` if non-bool.
    pub fn opt_bool(&self, idx: usize, kw_name: &str) -> Result<Option<bool>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(None);
        };
        parse_strict_bool(rep, &arg_label(idx, kw_name)).map(Some)
    }

    /// Extracts a required boolean or integer `0`/`1` (`PyRep::Bool` or `PyRep::Integer(0 | 1)`).
    pub fn req_boolish(&self, idx: usize, kw_name: &str) -> Result<bool, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required boolean {label}")))?;
        parse_boolish(rep, &label)
    }

    /// Extracts an optional boolean or integer `0`/`1` (`PyRep::Bool` or `PyRep::Integer(0 | 1)`).
    pub fn opt_boolish(&self, idx: usize, kw_name: &str) -> Result<Option<bool>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(None);
        };
        parse_boolish(rep, &arg_label(idx, kw_name)).map(Some)
    }

    /// Extracts a required string (`String`, `Token`, or UTF-8 `Bytes`).
    pub fn req_str(&self, idx: usize, kw_name: &str) -> Result<&'a str, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required string {label}")))?;
        parse_strict_str(rep, &label)
    }

    /// Extracts an optional string; returns `Ok(None)` if absent/`None`, or `Err` if malformed.
    pub fn opt_str(&self, idx: usize, kw_name: &str) -> Result<Option<&'a str>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(None);
        };
        parse_strict_str(rep, &arg_label(idx, kw_name)).map(Some)
    }

    /// Extracts a required non-empty, non-whitespace string.
    pub fn req_non_empty_str(&self, idx: usize, kw_name: &str) -> Result<&'a str, BridgeRpcError> {
        let s = self.req_str(idx, kw_name)?;
        if s.trim().is_empty() {
            return Err(invalid_arg(format!(
                "{} must not be empty",
                arg_label(idx, kw_name)
            )));
        }
        Ok(s)
    }

    /// Extracts a required list/tuple/set of `i64` values.
    pub fn req_i64_list(&self, idx: usize, kw_name: &str) -> Result<Vec<i64>, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required integer list {label}")))?;
        let items = match rep {
            PyRep::List(v) | PyRep::Tuple(v) | PyRep::Set(v) | PyRep::PySet(v) => v,
            _ => return Err(invalid_arg(format!("Expected list/tuple of integers for {label}"))),
        };
        items
            .iter()
            .enumerate()
            .map(|(i, el)| parse_strict_i64(el, &format!("{label}[{i}]")))
            .collect()
    }

    /// Extracts an optional list/tuple/set of `i64` values (returns empty `Vec` if absent/`None`).
    pub fn opt_i64_list(&self, idx: usize, kw_name: &str) -> Result<Vec<i64>, BridgeRpcError> {
        let Some(rep) = self.get_raw(idx, kw_name) else {
            return Ok(Vec::new());
        };
        let label = arg_label(idx, kw_name);
        let items = match rep {
            PyRep::List(v) | PyRep::Tuple(v) | PyRep::Set(v) | PyRep::PySet(v) => v,
            _ => return Err(invalid_arg(format!("Expected list/tuple of integers for {label}"))),
        };
        items
            .iter()
            .enumerate()
            .map(|(i, el)| parse_strict_i64(el, &format!("{label}[{i}]")))
            .collect()
    }

    /// Extracts either a single `i64` ID or a list/tuple/set of `i64` IDs.
    pub fn req_id_or_id_list(&self, idx: usize, kw_name: &str) -> Result<Vec<i64>, BridgeRpcError> {
        let label = arg_label(idx, kw_name);
        let rep = self
            .get_raw(idx, kw_name)
            .ok_or_else(|| invalid_arg(format!("Missing required ID or ID list {label}")))?;
        match rep {
            PyRep::Integer(id) => Ok(vec![*id]),
            PyRep::List(v) | PyRep::Tuple(v) | PyRep::Set(v) | PyRep::PySet(v) => v
                .iter()
                .enumerate()
                .map(|(i, el)| parse_strict_i64(el, &format!("{label}[{i}]")))
                .collect(),
            _ => Err(invalid_arg(format!(
                "Expected integer or list of integers for {label}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_and_opt_i64_positional_and_kwargs() {
        let mut kw = HashMap::new();
        kw.insert("itemID".to_string(), PyRep::Integer(42));
        kw.insert("badKw".to_string(), PyRep::String("nope".to_string()));

        let args = [PyRep::Integer(7), PyRep::None, PyRep::Bool(true)];
        let rpc = RpcArgs::new(&args, &kw);

        assert_eq!(rpc.req_i64(0, "itemID").unwrap(), 7);
        // Positional None falls back to kwarg
        assert_eq!(rpc.req_i64(1, "itemID").unwrap(), 42);
        // Out-of-bounds falls back to kwarg
        assert_eq!(rpc.req_i64(5, "itemID").unwrap(), 42);
        // Missing everywhere fails
        assert_eq!(rpc.req_i64(5, "missing").unwrap_err().code, "InvalidArgument");
        // Optional missing returns Ok(None)
        assert_eq!(rpc.opt_i64(5, "missing").unwrap(), None);
        // Malformed optional (Bool or String) returns Err(InvalidArgument), NOT Ok(None)
        assert_eq!(rpc.opt_i64(2, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.opt_i64(5, "badKw").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_i64(2, "").unwrap_err().code, "InvalidArgument");
    }

    #[test]
    fn positive_and_unsigned_integer_checks() {
        let args = [
            PyRep::Integer(0),
            PyRep::Integer(-5),
            PyRep::Integer(10),
            PyRep::Integer(i64::MAX),
        ];
        let rpc = RpcArgs::positional(&args);

        assert_eq!(rpc.req_positive_i64(0, "qty").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_positive_i64(1, "qty").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_positive_i64(2, "qty").unwrap(), 10);

        assert_eq!(rpc.req_u64(0, "u").unwrap(), 0);
        assert_eq!(rpc.req_u64(1, "u").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_u32(3, "u32").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_u32(2, "u32").unwrap(), 10);
    }

    #[test]
    fn finite_and_positive_f64_rejects_nan_inf_and_non_positive() {
        let mut kw = HashMap::new();
        kw.insert("price".to_string(), PyRep::Real(123.5));

        let args = [
            PyRep::Real(f64::NAN),
            PyRep::Real(f64::INFINITY),
            PyRep::Real(f64::NEG_INFINITY),
            PyRep::Real(0.0),
            PyRep::Real(-1.5),
            PyRep::Integer(50),
            PyRep::Bool(true),
        ];
        let rpc = RpcArgs::new(&args, &kw);

        for idx in 0..=2 {
            assert_eq!(rpc.req_finite_f64(idx, "").unwrap_err().code, "InvalidArgument");
            assert_eq!(rpc.opt_finite_f64(idx, "").unwrap_err().code, "InvalidArgument");
            assert_eq!(rpc.req_positive_f64(idx, "").unwrap_err().code, "InvalidArgument");
        }
        assert_eq!(rpc.req_finite_f64(3, "").unwrap(), 0.0);
        assert_eq!(rpc.req_positive_f64(3, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_positive_f64(4, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_non_negative_f64(4, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_positive_f64(5, "").unwrap(), 50.0);
        assert_eq!(rpc.req_finite_f64(6, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_positive_f64(10, "price").unwrap(), 123.5);
    }

    #[test]
    fn finite_vec3_handles_tuples_scalars_ints_and_rejects_non_finite() {
        let tuple_ints = [PyRep::Tuple(vec![
            PyRep::Integer(100),
            PyRep::Real(-20.5),
            PyRep::Integer(0),
        ])];
        assert_eq!(
            RpcArgs::positional(&tuple_ints).req_finite_vec3(0).unwrap(),
            DVec3::new(100.0, -20.5, 0.0)
        );

        let scalar_args = [
            PyRep::Integer(1),
            PyRep::Real(2.5),
            PyRep::Integer(-3),
        ];
        assert_eq!(
            RpcArgs::positional(&scalar_args).req_finite_vec3(0).unwrap(),
            DVec3::new(1.0, 2.5, -3.0)
        );

        let nan_tuple = [PyRep::List(vec![
            PyRep::Real(1.0),
            PyRep::Real(f64::NAN),
            PyRep::Real(3.0),
        ])];
        assert_eq!(
            RpcArgs::positional(&nan_tuple).req_finite_vec3(0).unwrap_err().code,
            "InvalidArgument"
        );

        let inf_scalars = [PyRep::Real(1.0), PyRep::Real(2.0), PyRep::Real(f64::INFINITY)];
        assert_eq!(
            RpcArgs::positional(&inf_scalars).req_finite_vec3(0).unwrap_err().code,
            "InvalidArgument"
        );

        let short_tuple = [PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(2.0)])];
        assert_eq!(
            RpcArgs::positional(&short_tuple).req_finite_vec3(0).unwrap_err().code,
            "InvalidArgument"
        );

        let none_vec = [PyRep::None];
        assert_eq!(RpcArgs::positional(&none_vec).opt_finite_vec3(0).unwrap(), None);
    }

    #[test]
    fn bool_strictness_rejects_truthy_integers_and_strings() {
        let mut kw = HashMap::new();
        kw.insert("accept".to_string(), PyRep::Bool(false));

        let args = [
            PyRep::Bool(true),
            PyRep::Integer(133_500_000_000_000_000),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::String("true".to_string()),
            PyRep::Integer(2),
            PyRep::Integer(-1),
        ];
        let rpc = RpcArgs::new(&args, &kw);

        assert!(rpc.req_bool(0, "").unwrap());
        assert!(!rpc.req_bool(10, "accept").unwrap());
        assert!(rpc.req_bool(2, "").unwrap());
        assert!(!rpc.req_bool(3, "").unwrap());
        assert_eq!(rpc.opt_bool(2, "").unwrap(), Some(true));
        assert_eq!(rpc.opt_bool(3, "").unwrap(), Some(false));
        for idx in [1, 4, 5, 6] {
            assert_eq!(rpc.req_bool(idx, "").unwrap_err().code, "InvalidArgument");
            assert_eq!(rpc.opt_bool(idx, "").unwrap_err().code, "InvalidArgument");
        }
        assert_eq!(rpc.opt_bool(10, "missing").unwrap(), None);

        // req_boolish / opt_boolish accept Bool and Integer(0 | 1), reject other integers and strings
        assert!(rpc.req_boolish(0, "").unwrap());
        assert!(rpc.req_boolish(2, "").unwrap());
        assert!(!rpc.req_boolish(3, "").unwrap());
        assert_eq!(rpc.opt_boolish(2, "").unwrap(), Some(true));
        assert_eq!(rpc.opt_boolish(3, "").unwrap(), Some(false));
        assert_eq!(rpc.opt_boolish(10, "missing").unwrap(), None);
        assert_eq!(rpc.req_boolish(1, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.opt_boolish(4, "").unwrap_err().code, "InvalidArgument");
    }

    #[test]
    fn str_and_id_list_extractors() {
        let mut kw = HashMap::new();
        kw.insert("reason".to_string(), PyRep::String("gift".to_string()));

        let args = [
            PyRep::String("   ".to_string()),
            PyRep::Integer(123),
            PyRep::List(vec![PyRep::Integer(10), PyRep::Integer(20)]),
            PyRep::List(vec![PyRep::Integer(10), PyRep::String("bad".to_string())]),
        ];
        let rpc = RpcArgs::new(&args, &kw);

        assert_eq!(rpc.req_str(0, "").unwrap(), "   ");
        assert_eq!(rpc.req_non_empty_str(0, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.opt_str(1, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.req_str(5, "reason").unwrap(), "gift");
        assert_eq!(rpc.req_i64_list(2, "").unwrap(), vec![10, 20]);
        assert_eq!(rpc.req_i64_list(3, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(rpc.opt_i64_list(9, "missing").unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn metadata_aliases_optional_bounds_and_collection_variants_kill_boundary_mutants() {
        // 1. Container metadata accessors: `is_empty` must require BOTH args and kwargs empty
        let empty = RpcArgs::positional(&[]);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.args().is_empty());
        assert!(empty.kwargs().is_empty());

        let mut kw = HashMap::new();
        kw.insert("altID".to_string(), PyRep::Integer(77));
        kw.insert("noneKw".to_string(), PyRep::None);
        kw.insert("badAlias".to_string(), PyRep::String("bad".into()));
        let only_kw = RpcArgs::new(&[], &kw);
        assert!(!only_kw.is_empty());
        assert_eq!(only_kw.len(), 0);
        assert_eq!(only_kw.kwargs().len(), 3);

        let args_only = [PyRep::Integer(1)];
        let only_args = RpcArgs::positional(&args_only);
        assert!(!only_args.is_empty());
        assert_eq!(only_args.len(), 1);
        assert_eq!(only_args.args().len(), 1);

        // 2. `get_raw_any`, `req_i64_any`, `opt_i64_any`
        let none_then_pos = [PyRep::None, PyRep::Integer(99)];
        let any_rpc = RpcArgs::new(&none_then_pos, &kw);
        assert_eq!(
            any_rpc
                .req_i64_any(0, &["", "noneKw", "missing", "altID"])
                .unwrap(),
            77
        );
        assert_eq!(any_rpc.req_i64_any(1, &["altID"]).unwrap(), 99);
        assert_eq!(
            any_rpc.req_i64_any(0, &[]).unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(any_rpc.opt_i64_any(0, &["missing"]).unwrap(), None);
        assert_eq!(
            any_rpc.opt_i64_any(0, &["badAlias"]).unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            any_rpc.opt_i64_any(0, &["altID"]).unwrap(),
            Some(77)
        );

        // 3. Exact boundary coverage for opt_positive_i64, req_non_negative_i64, opt_u64, u32, i32
        let boundary_args = [
            PyRep::Integer(0),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(i64::from(u32::MAX)),
            PyRep::Integer(i64::from(u32::MAX) + 1),
            PyRep::Integer(i64::from(i32::MIN)),
            PyRep::Integer(i64::from(i32::MIN) - 1),
            PyRep::Integer(i64::from(i32::MAX)),
            PyRep::Integer(i64::from(i32::MAX) + 1),
            PyRep::Bool(false),
        ];
        let brpc = RpcArgs::positional(&boundary_args);
        assert_eq!(brpc.opt_positive_i64(20, "").unwrap(), None);
        assert_eq!(
            brpc.opt_positive_i64(0, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            brpc.opt_positive_i64(1, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(brpc.opt_positive_i64(2, "").unwrap(), Some(1));

        assert_eq!(brpc.req_non_negative_i64(0, "").unwrap(), 0);
        assert_eq!(
            brpc.req_non_negative_i64(1, "").unwrap_err().code,
            "InvalidArgument"
        );

        assert_eq!(brpc.opt_u64(20, "").unwrap(), None);
        assert_eq!(brpc.opt_u64(0, "").unwrap(), Some(0));
        assert_eq!(brpc.opt_u64(1, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.opt_u64(9, "").unwrap_err().code, "InvalidArgument");

        assert_eq!(brpc.req_u32(0, "").unwrap(), 0);
        assert_eq!(brpc.req_u32(3, "").unwrap(), u32::MAX);
        assert_eq!(brpc.req_u32(1, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.req_u32(4, "").unwrap_err().code, "InvalidArgument");

        assert_eq!(brpc.opt_u32(20, "").unwrap(), None);
        assert_eq!(brpc.opt_u32(3, "").unwrap(), Some(u32::MAX));
        assert_eq!(brpc.opt_u32(1, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.opt_u32(4, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.opt_u32(9, "").unwrap_err().code, "InvalidArgument");

        assert_eq!(brpc.req_i32(5, "").unwrap(), i32::MIN);
        assert_eq!(brpc.req_i32(7, "").unwrap(), i32::MAX);
        assert_eq!(brpc.req_i32(6, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.req_i32(8, "").unwrap_err().code, "InvalidArgument");

        assert_eq!(brpc.opt_i32(20, "").unwrap(), None);
        assert_eq!(brpc.opt_i32(5, "").unwrap(), Some(i32::MIN));
        assert_eq!(brpc.opt_i32(7, "").unwrap(), Some(i32::MAX));
        assert_eq!(brpc.opt_i32(6, "").unwrap_err().code, "InvalidArgument");
        assert_eq!(brpc.opt_i32(8, "").unwrap_err().code, "InvalidArgument");

        // 4. Optional float positivity / non-negativity exact boundaries
        let flt_args = [
            PyRep::Real(0.0),
            PyRep::Real(-0.001),
            PyRep::Real(0.5),
            PyRep::Real(f64::NAN),
        ];
        let frpc = RpcArgs::positional(&flt_args);
        assert_eq!(frpc.opt_positive_f64(10, "").unwrap(), None);
        assert_eq!(
            frpc.opt_positive_f64(0, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            frpc.opt_positive_f64(1, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(frpc.opt_positive_f64(2, "").unwrap(), Some(0.5));
        assert_eq!(
            frpc.opt_positive_f64(3, "").unwrap_err().code,
            "InvalidArgument"
        );

        assert_eq!(frpc.req_non_negative_f64(0, "").unwrap(), 0.0);
        assert_eq!(frpc.opt_non_negative_f64(10, "").unwrap(), None);
        assert_eq!(frpc.opt_non_negative_f64(0, "").unwrap(), Some(0.0));
        assert_eq!(
            frpc.opt_non_negative_f64(1, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            frpc.opt_non_negative_f64(3, "").unwrap_err().code,
            "InvalidArgument"
        );

        // 5. req_finite_vec3 / opt_finite_vec3 missing scalars, 4-tuple, malformed elements
        assert_eq!(
            RpcArgs::positional(&[]).req_finite_vec3(0).unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::Real(1.0)])
                .req_finite_vec3(0)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::Real(1.0), PyRep::Real(2.0)])
                .req_finite_vec3(0)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::Tuple(vec![PyRep::Real(1.0); 4])])
                .req_finite_vec3(0)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::Tuple(vec![
                PyRep::Real(f64::NAN),
                PyRep::Real(1.0),
                PyRep::Real(2.0),
            ])])
            .req_finite_vec3(0)
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(2.0),
                PyRep::Real(f64::NEG_INFINITY),
            ])])
            .req_finite_vec3(0)
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
        assert_eq!(
            RpcArgs::positional(&[PyRep::String("bad".into())])
                .opt_finite_vec3(0)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(RpcArgs::positional(&[]).opt_finite_vec3(0).unwrap(), None);

        // 6. non-empty str trimming + Set / PySet / Tuple / scalar ID list variants
        assert_eq!(
            RpcArgs::positional(&[PyRep::String(" ok ".into())])
                .req_non_empty_str(0, "")
                .unwrap(),
            " ok "
        );
        let list_args = [
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
            PyRep::Set(vec![PyRep::Integer(3)]),
            PyRep::PySet(vec![PyRep::Integer(4), PyRep::Integer(5)]),
            PyRep::Integer(99),
            PyRep::String("not-list".into()),
        ];
        let lrpc = RpcArgs::positional(&list_args);
        assert_eq!(lrpc.req_i64_list(0, "").unwrap(), vec![1, 2]);
        assert_eq!(lrpc.req_i64_list(1, "").unwrap(), vec![3]);
        assert_eq!(lrpc.req_i64_list(2, "").unwrap(), vec![4, 5]);
        assert_eq!(
            lrpc.req_i64_list(4, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(lrpc.opt_i64_list(0, "").unwrap(), vec![1, 2]);
        assert_eq!(lrpc.opt_i64_list(1, "").unwrap(), vec![3]);
        assert_eq!(lrpc.opt_i64_list(2, "").unwrap(), vec![4, 5]);
        assert_eq!(
            lrpc.opt_i64_list(4, "").unwrap_err().code,
            "InvalidArgument"
        );

        assert_eq!(lrpc.req_id_or_id_list(3, "").unwrap(), vec![99]);
        assert_eq!(lrpc.req_id_or_id_list(0, "").unwrap(), vec![1, 2]);
        assert_eq!(lrpc.req_id_or_id_list(1, "").unwrap(), vec![3]);
        assert_eq!(lrpc.req_id_or_id_list(2, "").unwrap(), vec![4, 5]);
        assert_eq!(
            lrpc.req_id_or_id_list(4, "").unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            lrpc.req_id_or_id_list(20, "").unwrap_err().code,
            "InvalidArgument"
        );

        // 7. `opt_str` Some/None paths + `get_raw` with empty `kw_name` vs `PyRep::None` in `kwargs`
        assert_eq!(
            RpcArgs::positional(&[PyRep::String("hello".into())])
                .opt_str(0, "msg")
                .unwrap(),
            Some("hello")
        );
        assert_eq!(only_kw.opt_str(0, "noneKw").unwrap(), None);
        assert_eq!(only_kw.get_raw(0, ""), None);
        assert_eq!(only_kw.get_raw(0, "noneKw"), None);
        assert_eq!(only_kw.get_raw(0, "altID"), Some(&PyRep::Integer(77)));
        assert_eq!(
            RpcArgs::positional(&[PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("bad".into()),
            ])])
            .req_id_or_id_list(0, "ids")
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
    }
}
