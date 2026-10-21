//! Shared independent raw client-file readers for the `T-2` SDE sweeps (`cfsd` FSD binaries,
//! Python-2 pickles, magic-byte format detection). Test-only: nothing here reads the importer's
//! own `sde_resources` table to obtain a record count; counts are parsed from the client bytes.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub fn resfiles_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    let dir =
        PathBuf::from(&home).join("Library/Application Support/EVE Online/SharedCache/ResFiles");
    assert!(
        dir.is_dir(),
        "V24.01 client SharedCache/ResFiles directory must exist at {dir:?}"
    );
    dir
}

/// Pure-Rust RFC 1321 MD5 digest, used to pin each raw client file to its ledger md5.
pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let compress = |state: &mut [u32; 4], chunk: &[u8]| {
        let m: Vec<u32> = (0..16)
            .map(|i| u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap()))
            .collect();
        let [mut a, mut b, mut c, mut d] = *state;
        for r in 0..64 {
            let (f, g) = match r / 16 {
                0 => ((b & c) | (!b & d), r),
                1 => ((d & b) | (!d & c), (5 * r + 1) & 15),
                2 => (b ^ c ^ d, (3 * r + 5) & 15),
                _ => (c ^ (b | !d), (7 * r) & 15),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k[r]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[r]));
            a = temp;
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    };
    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let chunks = data.chunks_exact(64);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress(&mut state, chunk);
    }
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let tail_len = if rem.len() < 56 { 64 } else { 128 };
    tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_le_bytes());
    for chunk in tail[..tail_len].chunks_exact(64) {
        compress(&mut state, chunk);
    }
    state
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn u32_at(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("u32 slice"))
}

pub fn i32_at(buf: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(buf[pos..pos + 4].try_into().expect("i32 slice"))
}

pub fn u64_at(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

pub fn f64_at(buf: &[u8], pos: usize) -> f64 {
    f64::from_le_bytes(buf[pos..pos + 8].try_into().expect("f64 slice"))
}

/// Absolute position of a `cfsd` relative offset (offsets are relative to byte 32).
pub fn cfsd_abs(rel: u64) -> usize {
    32 + rel as usize
}

/// `cfsd` string: u64 LE byte length stored immediately before the UTF-8 bytes. `None` when the
/// relative offset is 0 (absent).
pub fn cfsd_str(buf: &[u8], rel: u64) -> Option<String> {
    if rel == 0 {
        return None;
    }
    let abs = cfsd_abs(rel);
    let len = u64_at(buf, abs - 8) as usize;
    Some(
        std::str::from_utf8(&buf[abs..abs + len])
            .expect("valid UTF-8 cfsd string")
            .to_string(),
    )
}

/// `cfsd` list: u64 LE element count before the elements. Returns (element start, count).
pub fn cfsd_list(buf: &[u8], rel: u64) -> (usize, usize) {
    if rel == 0 {
        return (0, 0);
    }
    let abs = cfsd_abs(rel);
    (abs, u64_at(buf, abs - 8) as usize)
}

/// Number of entries in a nested `cfsd` hash dict: the u64 bucket count precedes the bucket
/// offset table, and every non-empty bucket is preceded by its own u64 entry count.
pub fn cfsd_dict_len(buf: &[u8], rel: u64) -> usize {
    if rel == 0 {
        return 0;
    }
    let abs = cfsd_abs(rel);
    let buckets = u64_at(buf, abs - 8) as usize;
    (0..buckets)
        .map(|b| u64_at(buf, abs + b * 8))
        .filter(|&off| off != 0)
        .map(|off| u64_at(buf, cfsd_abs(off) - 8) as usize)
        .sum()
}

/// Raw client `.fsdbinary` file with its top-level records located by walking the hash buckets.
pub struct CfsdFile {
    pub raw: Vec<u8>,
    pub header_count: usize,
    pub records: Vec<usize>,
}

/// Reads `ResFiles/<cdn_relative_path>`, checks it is a `cfsd` container (u64 payload length at
/// byte 24 equals `len - 32`), pins its byte size and md5 to the ledger values, and walks every
/// top-level hash bucket at the given fixed record `stride`.
pub fn load_cfsd(
    cdn_relative_path: &str,
    ledger_md5: &str,
    ledger_bytes: u64,
    stride: usize,
) -> CfsdFile {
    let path = resfiles_dir().join(cdn_relative_path);
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    assert_eq!(raw.len() as u64, ledger_bytes, "{cdn_relative_path} size");
    assert_eq!(md5_hex(&raw), ledger_md5, "{cdn_relative_path} md5");
    assert_eq!(detect_format(&raw, false), "FSD Binary");
    let header_count = u64_at(&raw, 40) as usize;
    let buckets = u64_at(&raw, 48) as usize;
    let mut records = Vec::with_capacity(header_count);
    for b in 0..buckets {
        let off = u64_at(&raw, 56 + b * 8);
        if off == 0 {
            continue;
        }
        let abs = cfsd_abs(off);
        let n = u64_at(&raw, abs - 8) as usize;
        records.extend((0..n).map(|i| abs + i * stride));
    }
    assert_eq!(records.len(), header_count, "{cdn_relative_path} buckets");
    CfsdFile {
        raw,
        header_count,
        records,
    }
}

/// Container format detected from the file's own magic bytes (mirrors the importer's
/// `detect_resource_format`, written independently here).
pub fn detect_format(raw: &[u8], has_sibling_schema: bool) -> &'static str {
    if raw.starts_with(b"SQLite format 3\0") {
        return "SQLite3";
    }
    if raw.len() >= 40 && u64_at(raw, 24) == raw.len() as u64 - 32 {
        return "FSD Binary";
    }
    if raw.len() >= 8 {
        let n = u32_at(raw, 0) as usize;
        if n > 4 && n < raw.len() - 4 && &raw[4..7] == b"(dp" && raw[3 + n] == b'.' {
            return "FSD Static (pickled schema header)";
        }
    }
    if raw.starts_with(b"\x80\x02") {
        return "Pickle (protocol 2)";
    }
    if raw.first() == Some(&b'(') && raw.trim_ascii_end().last() == Some(&b'.') {
        return "Pickle (protocol 0)";
    }
    if !raw.is_empty()
        && raw
            .iter()
            .all(|&c| (32..127).contains(&c) || matches!(c, 9 | 10 | 13))
    {
        return "FSD Schema (YAML text)";
    }
    assert!(
        has_sibling_schema,
        "unrecognised client container {:02x?}",
        &raw[..16]
    );
    "FSD Static (external .schema)"
}

/// Decoded Python-2 pickle value (only the opcodes the V24.01 client resources use).
#[derive(Debug, Clone)]
pub enum PyVal {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Tuple(Vec<PyVal>),
    List(Rc<RefCell<Vec<PyVal>>>),
    Dict(Rc<RefCell<Vec<(PyVal, PyVal)>>>),
    Global(String),
}

impl PyVal {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            PyVal::Int(i) => Some(*i),
            PyVal::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyVal::Str(s) => Some(s),
            _ => None,
        }
    }
    /// Number of entries for dict/list/tuple values.
    pub fn len(&self) -> Option<usize> {
        match self {
            PyVal::Dict(d) => Some(d.borrow().len()),
            PyVal::List(l) => Some(l.borrow().len()),
            PyVal::Tuple(t) => Some(t.len()),
            _ => None,
        }
    }
    /// Looks up a string-keyed entry of a dict value.
    pub fn get(&self, key: &str) -> Option<PyVal> {
        match self {
            PyVal::Dict(d) => d
                .borrow()
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v.clone()),
            _ => None,
        }
    }
}

fn py_line(data: &[u8], pos: &mut usize) -> String {
    let end = *pos + data[*pos..].iter().position(|&c| c == b'\n').expect("EOL");
    let s = String::from_utf8_lossy(&data[*pos..end]).into_owned();
    *pos = end + 1;
    s
}

/// Undo Python 2 `repr()` string escaping used by the protocol-0 `S` opcode.
fn py_unrepr(s: &str) -> String {
    let inner = &s[1..s.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next().expect("escape") {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'x' => {
                let h: String = [it.next().unwrap(), it.next().unwrap()].iter().collect();
                out.push(char::from(u8::from_str_radix(&h, 16).expect("hex escape")));
            }
            other => out.push(other),
        }
    }
    out
}

/// Decode `raw-unicode-escape` (protocol-0 `V` opcode): only `\uXXXX`/`\UXXXXXXXX` are escapes.
fn py_raw_unicode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let width = match (bytes[i], bytes.get(i + 1)) {
            (b'\\', Some(b'u')) => 4,
            (b'\\', Some(b'U')) => 8,
            _ => 0,
        };
        if width > 0 {
            let hex = std::str::from_utf8(&bytes[i + 2..i + 2 + width]).expect("hex");
            let cp = u32::from_str_radix(hex, 16).expect("codepoint");
            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            i += 2 + width;
        } else {
            out.push(char::from(bytes[i]));
            i += 1;
        }
    }
    out
}

/// Minimal Python-2 unpickler (protocols 0 and 2) for the client's `.pickle` resources and the
/// pickled schema headers of `.static` files. Unknown opcodes panic so a format change fails loudly.
pub fn unpickle(data: &[u8]) -> PyVal {
    let mut stack: Vec<PyVal> = Vec::new();
    let mut marks: Vec<usize> = Vec::new();
    let mut memo: HashMap<u64, PyVal> = HashMap::new();
    let mut pos = 0usize;
    let pop_mark = |stack: &mut Vec<PyVal>, marks: &mut Vec<usize>| -> Vec<PyVal> {
        let m = marks.pop().expect("MARK");
        stack.split_off(m)
    };
    let set_items = |target: &PyVal, items: Vec<PyVal>| {
        let PyVal::Dict(d) = target else {
            panic!("SETITEM on non-dict")
        };
        let mut d = d.borrow_mut();
        let mut it = items.into_iter();
        while let (Some(k), Some(v)) = (it.next(), it.next()) {
            d.push((k, v));
        }
    };
    let append_items = |target: &PyVal, items: Vec<PyVal>| {
        let PyVal::List(l) = target else {
            panic!("APPEND on non-list")
        };
        l.borrow_mut().extend(items);
    };
    loop {
        let op = data[pos];
        pos += 1;
        match op {
            b'\x80' => pos += 1,
            b'(' => marks.push(stack.len()),
            b'.' => return stack.pop().expect("STOP value"),
            b'N' => stack.push(PyVal::None),
            b'\x88' | b'\x89' => stack.push(PyVal::Bool(op == b'\x88')),
            b'I' => {
                let l = py_line(data, &mut pos);
                stack.push(match l.as_str() {
                    "00" => PyVal::Bool(false),
                    "01" => PyVal::Bool(true),
                    s => PyVal::Int(s.parse().expect("INT")),
                });
            }
            b'L' => {
                let l = py_line(data, &mut pos);
                stack.push(PyVal::Int(l.trim_end_matches('L').parse().expect("LONG")));
            }
            b'F' => stack.push(PyVal::Float(
                py_line(data, &mut pos).parse().expect("FLOAT"),
            )),
            b'S' => stack.push(PyVal::Str(py_unrepr(&py_line(data, &mut pos)))),
            b'V' => {
                let end = pos + data[pos..].iter().position(|&c| c == b'\n').unwrap();
                stack.push(PyVal::Str(py_raw_unicode(&data[pos..end])));
                pos = end + 1;
            }
            b'J' => {
                stack.push(PyVal::Int(i64::from(i32_at(data, pos))));
                pos += 4;
            }
            b'K' => {
                stack.push(PyVal::Int(i64::from(data[pos])));
                pos += 1;
            }
            b'M' => {
                stack.push(PyVal::Int(i64::from(u16::from_le_bytes([
                    data[pos],
                    data[pos + 1],
                ]))));
                pos += 2;
            }
            b'\x8a' => {
                let n = data[pos] as usize;
                let mut v: i64 = 0;
                for i in (0..n).rev() {
                    v = (v << 8) | i64::from(data[pos + 1 + i]);
                }
                if n > 0 && n < 8 && data[pos + n] & 0x80 != 0 {
                    v -= 1i64 << (8 * n);
                }
                stack.push(PyVal::Int(v));
                pos += 1 + n;
            }
            b'G' => {
                let b: [u8; 8] = data[pos..pos + 8].try_into().unwrap();
                stack.push(PyVal::Float(f64::from_be_bytes(b)));
                pos += 8;
            }
            b'U' => {
                let n = data[pos] as usize;
                let s = data[pos + 1..pos + 1 + n].iter().map(|&c| char::from(c));
                stack.push(PyVal::Str(s.collect()));
                pos += 1 + n;
            }
            b'X' => {
                let n = u32_at(data, pos) as usize;
                let s = String::from_utf8_lossy(&data[pos + 4..pos + 4 + n]).into_owned();
                stack.push(PyVal::Str(s));
                pos += 4 + n;
            }
            b't' => {
                let items = pop_mark(&mut stack, &mut marks);
                stack.push(PyVal::Tuple(items));
            }
            b')' => stack.push(PyVal::Tuple(Vec::new())),
            b'\x85' | b'\x86' | b'\x87' => {
                let n = usize::from(op - 0x84);
                let items = stack.split_off(stack.len() - n);
                stack.push(PyVal::Tuple(items));
            }
            b'l' => {
                let items = pop_mark(&mut stack, &mut marks);
                stack.push(PyVal::List(Rc::new(RefCell::new(items))));
            }
            b']' => stack.push(PyVal::List(Rc::new(RefCell::new(Vec::new())))),
            b'd' => {
                let items = pop_mark(&mut stack, &mut marks);
                let dict = PyVal::Dict(Rc::new(RefCell::new(Vec::new())));
                set_items(&dict, items);
                stack.push(dict);
            }
            b'}' => stack.push(PyVal::Dict(Rc::new(RefCell::new(Vec::new())))),
            b's' => {
                let v = stack.pop().unwrap();
                let k = stack.pop().unwrap();
                set_items(stack.last().unwrap(), vec![k, v]);
            }
            b'u' => {
                let items = pop_mark(&mut stack, &mut marks);
                set_items(stack.last().unwrap(), items);
            }
            b'a' => {
                let v = stack.pop().unwrap();
                append_items(stack.last().unwrap(), vec![v]);
            }
            b'e' => {
                let items = pop_mark(&mut stack, &mut marks);
                append_items(stack.last().unwrap(), items);
            }
            b'p' => {
                let idx = py_line(data, &mut pos).parse().expect("PUT");
                memo.insert(idx, stack.last().unwrap().clone());
            }
            b'q' | b'r' => {
                let (idx, w) = if op == b'q' {
                    (u64::from(data[pos]), 1)
                } else {
                    (u64::from(u32_at(data, pos)), 4)
                };
                memo.insert(idx, stack.last().unwrap().clone());
                pos += w;
            }
            b'g' => {
                let idx: u64 = py_line(data, &mut pos).parse().expect("GET");
                stack.push(memo[&idx].clone());
            }
            b'h' | b'j' => {
                let (idx, w) = if op == b'h' {
                    (u64::from(data[pos]), 1)
                } else {
                    (u64::from(u32_at(data, pos)), 4)
                };
                stack.push(memo[&idx].clone());
                pos += w;
            }
            b'0' => {
                stack.pop();
            }
            b'c' => {
                let module = py_line(data, &mut pos);
                let name = py_line(data, &mut pos);
                stack.push(PyVal::Global(format!("{module}.{name}")));
            }
            b'R' => {
                let args = stack.pop().unwrap();
                let func = stack.pop().unwrap();
                let PyVal::Global(g) = func else {
                    panic!("REDUCE on {func:?}")
                };
                assert_eq!(g, "collections.OrderedDict", "unsupported REDUCE callable");
                let dict = PyVal::Dict(Rc::new(RefCell::new(Vec::new())));
                if let PyVal::Tuple(a) = args {
                    if let Some(PyVal::List(pairs)) = a.first() {
                        for p in pairs.borrow().iter() {
                            // Python 2 `OrderedDict.__reduce__` emits `[key, value]` lists.
                            let kv = match p {
                                PyVal::Tuple(kv) => kv.clone(),
                                PyVal::List(kv) => kv.borrow().clone(),
                                other => panic!("OrderedDict item {other:?}"),
                            };
                            set_items(&dict, kv);
                        }
                    }
                }
                stack.push(dict);
            }
            other => panic!("unsupported pickle opcode 0x{other:02x} at byte {}", pos - 1),
        }
    }
}

/// English labels from `localization_fsd_en-us.pickle` (`('en-us', {messageID: (text, ...)})`),
/// keeping only non-empty texts, exactly like the importer's `label()` lookup.
pub fn en_us_labels(raw: &[u8]) -> HashMap<u32, String> {
    let top = unpickle(raw);
    let PyVal::Tuple(t) = top else {
        panic!("localization pickle must be a tuple")
    };
    assert_eq!(t[0].as_str(), Some("en-us"));
    let PyVal::Dict(d) = &t[1] else {
        panic!("localization message dict")
    };
    let mut out = HashMap::with_capacity(d.borrow().len());
    for (k, v) in d.borrow().iter() {
        if let (Some(id), PyVal::Tuple(parts)) = (k.as_int(), v) {
            if let Some(text) = parts.first().and_then(PyVal::as_str) {
                if !text.is_empty() {
                    out.insert(id as u32, text.to_string());
                }
            }
        }
    }
    out
}
