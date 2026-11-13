//! Runtime lock-rank enforcement and ratchet checking (`RFC-1 §4`).
//!
//! Assigns static numeric ranks to synchronization primitives (`Ranked<L, RANK>`)
//! and maintains a thread-local stack of currently held lock ranks across debug,
//! test, and release builds. Acquiring a lock whose rank is less than or equal
//! to any currently held lock on the same thread is reported with both the outer
//! held call site and the inner acquisition call site, and matched against
//! `lock_rank_ratchet.txt` via stable `(relative_file, enclosing_fn, field_name)` keys.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LockResult, Mutex, OnceLock, PoisonError, RwLock, TryLockError, TryLockResult};

/// Rank `5`: per-system NPC spawner map (`ServerContext::npc_spawns`), outer to
/// `SystemRegistry::systems` (`10`), `Ballpark` (`20`), and `WorldRepository::inner` (`40`).
pub const RANK_NPC_SPAWNS: u16 = 5;
/// Rank `10`: solar system actor registry (`SystemRegistry::systems`), outer to
/// `Ballpark` (`20`), `ActorState` (`21`), `Crimewatch` (`30`), and `WorldRepository::inner` (`40`).
pub const RANK_SYSTEM_REGISTRY: u16 = 10;
/// Rank `20`: per-system `Ballpark` (`SystemHandle::ballpark`).
pub const RANK_BALLPARK: u16 = 20;
/// Rank `21`: per-system `ActorState` (`SystemHandle::actor_state`).
pub const RANK_ACTOR_STATE: u16 = 21;
/// Rank `30`: transitional CrimeWatch coordinator lock (`CrimeWatchLock::lock`).
pub const RANK_CRIMEWATCH: u16 = 30;
/// Rank `35`: service-level registries (`SystemRegistry::cluster_nodes`) that read `WorldRepository` (`40`).
pub const RANK_SERVICE_STATE: u16 = 35;
/// Rank `40`: `WorldRepository::inner` snapshot lock.
pub const RANK_WORLD_INNER: u16 = 40;
/// Rank `41`: `WorldRepository::sqlite_conn`.
pub const RANK_SQLITE_CONN: u16 = 41;
/// Rank `42`: `WorldRepository::sqlite_fp`.
pub const RANK_SQLITE_FP: u16 = 42;
/// Rank `43`: `WorldRepository::wal_lock`.
pub const RANK_WAL_LOCK: u16 = 43;
/// Rank `60`: leaf locks (`WorldRepository::auth_runtime`, `DedicatedAuthConn::conn`, `SystemHandle`
/// caches, runtime maps, notification queues, `bound_objects`, `esi_state.*`, and Quasar meta).
pub const RANK_LEAF: u16 = 60;
/// Rank `70`: terminal clock pointer lock (`WorldRepository::clock`, `ServerContext::clock`);
/// never acquires any inner lock while held, so leaf locks may read the clock.
pub const RANK_CLOCK: u16 = 70;

/// Generated ratchet baseline of known temporary lock-rank violations (`(held, acquired, site)`).
/// When empty, default enforcement switches to `panic`.
pub const LOCK_RANK_RATCHET_RAW: &str = include_str!("../lock_rank_ratchet.txt");

/// Runtime checking mode controlled by `EVOMU_LOCK_RANK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockRankMode {
    /// Panic immediately on any lock-rank inversion or assertion failure.
    Panic,
    /// Allow triples already present in `lock_rank_ratchet.txt`, panic on any new triple.
    EnforceRatchet,
    /// Record unique `(held, acquired, site)` triples to the ratchet file without panicking.
    Record,
    /// Emit warnings to stderr/tracing without panicking.
    Warn,
}

/// Total number of unratcheted lock-rank violations observed in this process.
///
/// Incremented before panicking (or warning when configured) so callers using
/// `catch_unwind` or task supervision cannot silently swallow lock-rank violations.
pub fn unratcheted_violation_count() -> u64 {
    imp::UNRATCHETED_VIOLATIONS.load(Ordering::Relaxed)
}

/// Structured, line-number-independent ratchet key:
/// `<relative_file>::<enclosing_fn_name>::<field_name>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RatchetSiteKey {
    /// Repository-relative source file path with `/` separators.
    pub rel_file: String,
    /// Enclosing function name (or `<module>`).
    pub enclosing_fn: String,
    /// Target lock/snapshot field or helper method name.
    pub field_name: String,
}

impl RatchetSiteKey {
    /// Parses `<relative_file>::<enclosing_fn_name>::<field_name>`.
    ///
    /// Returns `None` for legacy `file:line:col` keys or malformed strings.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split("::");
        let rel_file = parts.next()?.trim();
        let enclosing_fn = parts.next()?.trim();
        let field_name = parts.next()?.trim();
        if parts.next().is_some()
            || rel_file.is_empty()
            || enclosing_fn.is_empty()
            || field_name.is_empty()
            || rel_file.contains(':')
            || enclosing_fn.contains(':')
            || field_name.contains(':')
        {
            return None;
        }
        Some(Self {
            rel_file: normalize_rel_file(rel_file),
            enclosing_fn: enclosing_fn.to_string(),
            field_name: field_name.to_string(),
        })
    }
}

impl fmt::Display for RatchetSiteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}::{}::{}",
            self.rel_file, self.enclosing_fn, self.field_name
        )
    }
}

/// Parsed entry from `lock_rank_ratchet.txt`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RatchetEntry {
    /// Rank of the already-held outer lock.
    pub held_rank: u16,
    /// Rank of the inner lock being acquired (or upper bound for `assert_locks_below`).
    pub acquired_rank: u16,
    /// Structured site key (`<relative_file>::<enclosing_fn_name>::<field_name>`).
    pub key: RatchetSiteKey,
}

fn normalize_rel_file(raw_file: &str) -> String {
    let file = raw_file.replace('\\', "/");
    if let Some(idx) = file.find("/crates/") {
        file[idx + 1..].to_string()
    } else if let Some(rest) = file.strip_prefix("./") {
        rest.to_string()
    } else {
        file
    }
}

/// Returns the workspace root directory containing `crates/`.
pub fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(Path::parent) {
        if root.join("crates").is_dir() {
            return root.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Strictly parses all non-comment lines in `raw`, rejecting duplicates and legacy `file:line:col` keys.
pub fn parse_ratchet_entries(raw: &str) -> Result<Vec<RatchetEntry>, String> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for (idx, raw_line) in raw.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split('\t');
        let (Some(h), Some(a), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(format!(
                "line {line_no}: expected 3 tab-separated columns `<held>\\t<acq>\\t<file>::<fn>::<field>`, got: {trimmed:?}"
            ));
        };
        let held_rank = h
            .parse::<u16>()
            .map_err(|_| format!("line {line_no}: invalid held_rank {h:?}"))?;
        let acquired_rank = a
            .parse::<u16>()
            .map_err(|_| format!("line {line_no}: invalid acquired_rank {a:?}"))?;
        let key = RatchetSiteKey::parse(s).ok_or_else(|| {
            format!(
                "line {line_no}: ratchet key must use structured `<relative_file>::<enclosing_fn_name>::<field_name>` format (legacy `file:line:col` keys are rejected), got: {s:?}"
            )
        })?;
        let entry = RatchetEntry {
            held_rank,
            acquired_rank,
            key,
        };
        if !seen.insert(entry.clone()) {
            return Err(format!(
                "line {line_no}: duplicate ratchet entry `{held_rank}\\t{acquired_rank}\\t{}`",
                entry.key
            ));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Validates a ratchet file's policy, structured key syntax, and live workspace sites.
///
/// Fails loudly if any entry uses legacy `file:line:col` keys, duplicates another entry,
/// violates forbidden rank transitions, points at a missing file, or matches 0 live lock sites.
pub fn validate_ratchet_against_workspace(raw: &str, root: &Path) -> Result<usize, Vec<String>> {
    let entries = match parse_ratchet_entries(raw) {
        Ok(e) => e,
        Err(err) => return Err(vec![err]),
    };

    let mut errors = Vec::new();
    let mut file_cache: HashMap<String, Vec<imp::ScannedLockSite>> = HashMap::new();

    for entry in &entries {
        if entry.held_rank == RANK_BALLPARK && entry.acquired_rank == RANK_BALLPARK {
            errors.push(format!(
                "forbidden 20 -> 20 same-rank Ballpark ratchet entry: {}",
                entry.key
            ));
        }
        if entry.held_rank == RANK_SQLITE_CONN
            || entry.acquired_rank == RANK_SQLITE_CONN
            || entry.held_rank == RANK_SQLITE_FP
            || entry.acquired_rank == RANK_SQLITE_FP
        {
            errors.push(format!(
                "stale RANK_SQLITE_CONN/FP ({}/{}) entry: {} -> {} {}",
                RANK_SQLITE_CONN, RANK_SQLITE_FP, entry.held_rank, entry.acquired_rank, entry.key
            ));
        }
        if entry.held_rank == RANK_LEAF
            && (entry.acquired_rank == RANK_BALLPARK || entry.acquired_rank == RANK_ACTOR_STATE)
        {
            errors.push(format!(
                "forbidden 60 -> {} ratchet entry: {}",
                entry.acquired_rank, entry.key
            ));
        }

        let rel = &entry.key.rel_file;
        if !file_cache.contains_key(rel) {
            let full = root.join(rel);
            match std::fs::read_to_string(&full) {
                Ok(src) => {
                    file_cache.insert(rel.clone(), imp::scan_file_sites(&src).sites);
                }
                Err(err) => {
                    errors.push(format!(
                        "ratchet entry {} -> {} {} references missing/unreadable file {}: {err}",
                        entry.held_rank,
                        entry.acquired_rank,
                        entry.key,
                        full.display()
                    ));
                    continue;
                }
            }
        }

        if let Some(sites) = file_cache.get(rel) {
            let matched = sites.iter().any(|site| {
                site.enclosing_fn == entry.key.enclosing_fn
                    && site.field_name == entry.key.field_name
            });
            if !matched {
                errors.push(format!(
                    "stale ratchet entry matches 0 live lock sites: {} -> {} {}",
                    entry.held_rank, entry.acquired_rank, entry.key
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(entries.len())
    } else {
        Err(errors)
    }
}

mod imp {
    use super::{
        normalize_rel_file, parse_ratchet_entries, workspace_root, AtomicU64, BTreeSet, HashMap,
        HashSet, Location, LockRankMode, Mutex, OnceLock, Ordering, PathBuf, RatchetSiteKey,
        LOCK_RANK_RATCHET_RAW,
    };
    #[cfg(test)]
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::thread::ThreadId;

    pub(crate) static UNRATCHETED_VIOLATIONS: AtomicU64 = AtomicU64::new(0);

    const LOCK_METHODS: &[&str] = &[
        "read",
        "write",
        "lock",
        "try_read",
        "try_write",
        "try_lock",
        "read_unranked",
        "try_write_unranked",
        "with_snapshot",
        "try_with_snapshot",
        "with_mut_snapshot",
        "try_with_mut_snapshot",
        "tx",
        "read_state",
        "try_read_state",
        "assert_no_locks_held",
        "assert_locks_below",
    ];

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum TokKind {
        Ident,
        RawIdent,
        Punct,
        Other,
    }

    struct Tok {
        kind: TokKind,
        text: String,
        line: u32,
        col: u32,
        end_col: u32,
    }

    struct FnSpan {
        name: String,
        start_line: u32,
        end_line: u32,
    }

    pub(crate) struct ScannedLockSite {
        pub(crate) enclosing_fn: String,
        pub(crate) field_name: String,
        recv_line: u32,
        recv_col: u32,
        method_line: u32,
        method_col: u32,
        method_end_col: u32,
    }

    pub(crate) struct ScannedFile {
        fn_spans: Vec<FnSpan>,
        pub(crate) sites: Vec<ScannedLockSite>,
    }

    fn tokenize_rust(src: &str) -> Vec<Tok> {
        let chars: Vec<char> = src.chars().collect();
        let n = chars.len();
        let mut i = 0usize;
        let mut line = 1u32;
        let mut col = 1u32;
        let mut tokens = Vec::new();

        let step = |i: &mut usize, line: &mut u32, col: &mut u32, k: usize| {
            for _ in 0..k {
                if *i >= n {
                    return;
                }
                if chars[*i] == '\n' {
                    *line += 1;
                    *col = 1;
                } else {
                    *col += 1;
                }
                *i += 1;
            }
        };

        while i < n {
            let c = chars[i];
            if c.is_whitespace() {
                step(&mut i, &mut line, &mut col, 1);
                continue;
            }
            if c == '/' && i + 1 < n && chars[i + 1] == '/' {
                while i < n && chars[i] != '\n' {
                    step(&mut i, &mut line, &mut col, 1);
                }
                continue;
            }
            if c == '/' && i + 1 < n && chars[i + 1] == '*' {
                let mut depth = 1usize;
                step(&mut i, &mut line, &mut col, 2);
                while i < n && depth > 0 {
                    if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                        depth += 1;
                        step(&mut i, &mut line, &mut col, 2);
                    } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                        depth -= 1;
                        step(&mut i, &mut line, &mut col, 2);
                    } else {
                        step(&mut i, &mut line, &mut col, 1);
                    }
                }
                continue;
            }
            if (c == 'r' || (c == 'b' && i + 1 < n && chars[i + 1] == 'r')) && i + 1 < n {
                if c == 'r' && chars[i + 1] == '#' {
                    let mut j = i + 1;
                    let mut hashes = 0usize;
                    while j < n && chars[j] == '#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < n && chars[j] == '"' {
                        let (tl, tc) = (line, col);
                        let count = j - i + 1;
                        step(&mut i, &mut line, &mut col, count);
                        while i < n {
                            if chars[i] == '"'
                                && i + hashes < n
                                && (1..=hashes).all(|h| chars[i + h] == '#')
                            {
                                step(&mut i, &mut line, &mut col, 1 + hashes);
                                break;
                            }
                            step(&mut i, &mut line, &mut col, 1);
                        }
                        tokens.push(Tok {
                            kind: TokKind::Other,
                            text: String::new(),
                            line: tl,
                            col: tc,
                            end_col: col.saturating_sub(1),
                        });
                        continue;
                    } else if hashes == 1
                        && j < n
                        && (chars[j].is_ascii_alphabetic() || chars[j] == '_')
                    {
                        let (tl, tc) = (line, col);
                        let start = i;
                        step(&mut i, &mut line, &mut col, 2);
                        while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                            step(&mut i, &mut line, &mut col, 1);
                        }
                        let text: String = chars[start..i].iter().collect();
                        tokens.push(Tok {
                            kind: TokKind::RawIdent,
                            text,
                            line: tl,
                            col: tc,
                            end_col: col.saturating_sub(1),
                        });
                        continue;
                    }
                } else {
                    let mut j = i + if c == 'b' { 2 } else { 1 };
                    let mut hashes = 0usize;
                    while j < n && chars[j] == '#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < n && chars[j] == '"' {
                        let (tl, tc) = (line, col);
                        let count = j - i + 1;
                        step(&mut i, &mut line, &mut col, count);
                        while i < n {
                            if chars[i] == '"'
                                && i + hashes < n
                                && (1..=hashes).all(|h| chars[i + h] == '#')
                            {
                                step(&mut i, &mut line, &mut col, 1 + hashes);
                                break;
                            }
                            step(&mut i, &mut line, &mut col, 1);
                        }
                        tokens.push(Tok {
                            kind: TokKind::Other,
                            text: String::new(),
                            line: tl,
                            col: tc,
                            end_col: col.saturating_sub(1),
                        });
                        continue;
                    }
                }
            }
            if c == '"' || (c == 'b' && i + 1 < n && chars[i + 1] == '"') {
                let (tl, tc) = (line, col);
                step(&mut i, &mut line, &mut col, if c == 'b' { 2 } else { 1 });
                while i < n {
                    if chars[i] == '\\' {
                        step(&mut i, &mut line, &mut col, 2);
                    } else if chars[i] == '"' {
                        step(&mut i, &mut line, &mut col, 1);
                        break;
                    } else {
                        step(&mut i, &mut line, &mut col, 1);
                    }
                }
                tokens.push(Tok {
                    kind: TokKind::Other,
                    text: String::new(),
                    line: tl,
                    col: tc,
                    end_col: col.saturating_sub(1),
                });
                continue;
            }
            if c == '\'' || (c == 'b' && i + 1 < n && chars[i + 1] == '\'') {
                let (tl, tc) = (line, col);
                if c == 'b' {
                    step(&mut i, &mut line, &mut col, 2);
                    while i < n {
                        if chars[i] == '\\' {
                            step(&mut i, &mut line, &mut col, 2);
                        } else if chars[i] == '\'' {
                            step(&mut i, &mut line, &mut col, 1);
                            break;
                        } else {
                            step(&mut i, &mut line, &mut col, 1);
                        }
                    }
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        text: String::new(),
                        line: tl,
                        col: tc,
                        end_col: col.saturating_sub(1),
                    });
                    continue;
                }
                if i + 1 < n && chars[i + 1] == '\\' {
                    step(&mut i, &mut line, &mut col, 2);
                    while i < n && chars[i] != '\'' && chars[i] != '\n' {
                        step(&mut i, &mut line, &mut col, 1);
                    }
                    if i < n && chars[i] == '\'' {
                        step(&mut i, &mut line, &mut col, 1);
                    }
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        text: String::new(),
                        line: tl,
                        col: tc,
                        end_col: col.saturating_sub(1),
                    });
                    continue;
                }
                if i + 2 < n && chars[i + 2] == '\'' {
                    step(&mut i, &mut line, &mut col, 3);
                    tokens.push(Tok {
                        kind: TokKind::Other,
                        text: String::new(),
                        line: tl,
                        col: tc,
                        end_col: col.saturating_sub(1),
                    });
                    continue;
                }
                let start = i;
                step(&mut i, &mut line, &mut col, 1);
                while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    step(&mut i, &mut line, &mut col, 1);
                }
                tokens.push(Tok {
                    kind: TokKind::Other,
                    text: chars[start..i].iter().collect(),
                    line: tl,
                    col: tc,
                    end_col: col.saturating_sub(1),
                });
                continue;
            }
            if c.is_ascii_alphabetic() || c == '_' {
                let (tl, tc) = (line, col);
                let start = i;
                while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    step(&mut i, &mut line, &mut col, 1);
                }
                tokens.push(Tok {
                    kind: TokKind::Ident,
                    text: chars[start..i].iter().collect(),
                    line: tl,
                    col: tc,
                    end_col: col.saturating_sub(1),
                });
                continue;
            }
            let (tl, tc) = (line, col);
            step(&mut i, &mut line, &mut col, 1);
            tokens.push(Tok {
                kind: TokKind::Punct,
                text: c.to_string(),
                line: tl,
                col: tc,
                end_col: tc,
            });
        }
        tokens
    }

    fn receiver_start(tokens: &[Tok], dot_idx: usize) -> (u32, u32) {
        let mut k = dot_idx;
        let mut start = (tokens[dot_idx].line, tokens[dot_idx].col);
        while k > 0 {
            let prev = &tokens[k - 1];
            match (prev.kind, prev.text.as_str()) {
                (TokKind::Ident | TokKind::RawIdent, _) => {
                    if matches!(
                        prev.text.as_str(),
                        "let" | "return" | "if" | "else" | "match" | "while" | "for" | "in" | "mut"
                    ) {
                        break;
                    }
                    start = (prev.line, prev.col);
                    k -= 1;
                }
                (TokKind::Punct, ".") | (TokKind::Punct, ":") => {
                    k -= 1;
                }
                (TokKind::Punct, ")") | (TokKind::Punct, "]") => {
                    let close = prev.text.as_str();
                    let open = if close == ")" { "(" } else { "[" };
                    let mut depth = 1usize;
                    k -= 1;
                    while k > 0 && depth > 0 {
                        k -= 1;
                        if tokens[k].kind == TokKind::Punct {
                            if tokens[k].text == close {
                                depth += 1;
                            } else if tokens[k].text == open {
                                depth -= 1;
                            }
                        }
                    }
                    start = (tokens[k].line, tokens[k].col);
                }
                _ => break,
            }
        }
        start
    }

    pub(crate) fn scan_file_sites(src: &str) -> ScannedFile {
        let tokens = tokenize_rust(src);
        let mut brace_depth = 0usize;
        let mut fn_stack: Vec<(String, u32, usize)> = Vec::new();
        let mut fn_spans: Vec<FnSpan> = Vec::new();
        let mut pending_fn: Option<(String, u32)> = None;
        let mut pending_paren = 0usize;
        let mut sites = Vec::new();

        for (idx, tok) in tokens.iter().enumerate() {
            if let Some((ref fn_name, fn_line)) = pending_fn.clone() {
                if tok.kind == TokKind::Punct {
                    match tok.text.as_str() {
                        "(" => pending_paren += 1,
                        ")" if pending_paren > 0 => pending_paren -= 1,
                        ";" if pending_paren == 0 => {
                            pending_fn = None;
                        }
                        "{" if pending_paren == 0 => {
                            brace_depth += 1;
                            fn_stack.push((fn_name.clone(), fn_line, brace_depth));
                            pending_fn = None;
                            continue;
                        }
                        _ => {}
                    }
                }
            }

            if tok.kind == TokKind::Punct {
                if tok.text == "{" {
                    brace_depth += 1;
                } else if tok.text == "}" {
                    brace_depth = brace_depth.saturating_sub(1);
                    while let Some(top) = fn_stack.last() {
                        if top.2 > brace_depth {
                            let (name, start_line, _) = fn_stack.pop().expect("non-empty");
                            fn_spans.push(FnSpan {
                                name,
                                start_line,
                                end_line: tok.line,
                            });
                        } else {
                            break;
                        }
                    }
                }
            } else if tok.kind == TokKind::Ident
                && tok.text == "fn"
                && pending_fn.is_none()
                && idx + 1 < tokens.len()
                && tokens[idx + 1].kind == TokKind::Ident
            {
                pending_fn = Some((tokens[idx + 1].text.clone(), tok.line));
                pending_paren = 0;
            }

            if tok.kind == TokKind::Ident && LOCK_METHODS.contains(&tok.text.as_str()) {
                let is_call = if idx + 1 < tokens.len() && tokens[idx + 1].kind == TokKind::Punct {
                    tokens[idx + 1].text == "("
                        || (tokens[idx + 1].text == ":"
                            && idx + 3 < tokens.len()
                            && tokens[idx + 2].kind == TokKind::Punct
                            && tokens[idx + 2].text == ":"
                            && tokens[idx + 3].kind == TokKind::Punct
                            && tokens[idx + 3].text == "<")
                } else {
                    false
                };
                if !is_call {
                    continue;
                }

                let enclosing_fn = fn_stack
                    .last()
                    .map(|(n, _, _)| n.clone())
                    .unwrap_or_else(|| "<module>".to_string());
                let mut field_name = tok.text.clone();
                let mut recv = (tok.line, tok.col);
                if idx >= 1 && tokens[idx - 1].kind == TokKind::Punct && tokens[idx - 1].text == "."
                {
                    recv = receiver_start(&tokens, idx - 1);
                    if idx >= 2 {
                        let prev = &tokens[idx - 2];
                        if matches!(prev.kind, TokKind::Ident | TokKind::RawIdent) {
                            field_name = if prev.text == "self" {
                                tok.text.clone()
                            } else {
                                prev.text.clone()
                            };
                        } else if prev.kind == TokKind::Punct && prev.text == ")" {
                            let mut pdepth = 1usize;
                            let mut k = idx.saturating_sub(3);
                            loop {
                                if tokens[k].kind == TokKind::Punct {
                                    if tokens[k].text == ")" {
                                        pdepth += 1;
                                    } else if tokens[k].text == "(" {
                                        pdepth -= 1;
                                        if pdepth == 0 {
                                            break;
                                        }
                                    }
                                }
                                if k == 0 {
                                    break;
                                }
                                k -= 1;
                            }
                            if k > 0
                                && pdepth == 0
                                && matches!(tokens[k - 1].kind, TokKind::Ident | TokKind::RawIdent)
                            {
                                field_name = tokens[k - 1].text.clone();
                            }
                        }
                    }
                }
                sites.push(ScannedLockSite {
                    enclosing_fn,
                    field_name,
                    recv_line: recv.0,
                    recv_col: recv.1,
                    method_line: tok.line,
                    method_col: tok.col,
                    method_end_col: tok.end_col,
                });
            }
        }

        ScannedFile { fn_spans, sites }
    }

    fn scanned_file_for_rel(rel_file: &str, raw_file: &str) -> Option<&'static ScannedFile> {
        static CACHE: OnceLock<Mutex<HashMap<String, Option<&'static ScannedFile>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        {
            let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(&cached) = guard.get(rel_file) {
                return cached;
            }
        }
        let candidates = [
            workspace_root().join(rel_file),
            PathBuf::from(rel_file),
            PathBuf::from(raw_file),
        ];
        let parsed = candidates
            .into_iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|src| Box::leak(Box::new(scan_file_sites(&src))) as &'static ScannedFile);
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard.entry(rel_file.to_string()).or_insert(parsed)
    }

    fn resolve_ratchet_key(site: &'static Location<'static>) -> Option<RatchetSiteKey> {
        let rel_file = normalize_rel_file(site.file());
        let scanned = scanned_file_for_rel(&rel_file, site.file())?;
        let target = (site.line(), site.column());

        // 1. Prefer a site whose receiver-through-method token span contains `target`.
        let best_span = scanned
            .sites
            .iter()
            .filter(|s| {
                (s.recv_line, s.recv_col) <= target
                    && target <= (s.method_line, s.method_end_col.saturating_add(1))
            })
            .max_by_key(|s| (s.recv_line, s.recv_col));
        if let Some(s) = best_span {
            return Some(RatchetSiteKey {
                rel_file,
                enclosing_fn: s.enclosing_fn.clone(),
                field_name: s.field_name.clone(),
            });
        }

        // 2. Fallback: closest site on the same line (e.g. macro/chained call).
        let same_line = scanned
            .sites
            .iter()
            .filter(|s| s.method_line == site.line() || s.recv_line == site.line())
            .min_by_key(|s| s.method_col.abs_diff(site.column()));
        if let Some(s) = same_line {
            return Some(RatchetSiteKey {
                rel_file,
                enclosing_fn: s.enclosing_fn.clone(),
                field_name: s.field_name.clone(),
            });
        }

        // 3. Fallback: enclosing function span from brace scan.
        let enclosing_fn = scanned
            .fn_spans
            .iter()
            .filter(|f| f.start_line <= site.line() && site.line() <= f.end_line)
            .min_by_key(|f| f.end_line.saturating_sub(f.start_line))
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "<module>".to_string());
        Some(RatchetSiteKey {
            rel_file,
            enclosing_fn,
            field_name: "unknown".to_string(),
        })
    }

    #[derive(Clone, Copy)]
    struct HeldEntry {
        id: u64,
        rank: u16,
        site: &'static Location<'static>,
    }

    #[derive(Default)]
    struct HeldStack {
        next_id: u64,
        entries: Vec<HeldEntry>,
    }

    fn held_stacks() -> &'static Mutex<HashMap<ThreadId, HeldStack>> {
        static HELD_BY_THREAD: OnceLock<Mutex<HashMap<ThreadId, HeldStack>>> = OnceLock::new();
        HELD_BY_THREAD.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) struct HeldToken {
        id: u64,
        thread_id: Option<ThreadId>,
    }

    impl HeldToken {
        pub(crate) const fn none() -> Self {
            Self {
                id: 0,
                thread_id: None,
            }
        }
    }

    impl Drop for HeldToken {
        fn drop(&mut self) {
            if self.id == 0 {
                return;
            }
            let Some(tid) = self.thread_id else {
                return;
            };
            let mut map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stack) = map.get_mut(&tid) {
                if let Some(pos) = stack.entries.iter().rposition(|e| e.id == self.id) {
                    stack.entries.remove(pos);
                }
            }
        }
    }

    #[cfg(test)]
    thread_local! {
        static TEST_CONFIG: RefCell<Option<(LockRankMode, Option<PathBuf>)>> = const { RefCell::new(None) };
    }

    pub(crate) fn format_location(site: &'static Location<'static>) -> String {
        let file = normalize_rel_file(site.file());
        format!("{file}:{}:{}", site.line(), site.column())
    }

    pub(crate) fn parse_ratchet(raw: &str) -> HashSet<(u16, u16, String)> {
        parse_ratchet_entries(raw)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|e| (e.held_rank, e.acquired_rank, e.key.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn baseline_ratchet() -> &'static HashSet<(u16, u16, String)> {
        static BASELINE: OnceLock<HashSet<(u16, u16, String)>> = OnceLock::new();
        BASELINE.get_or_init(|| parse_ratchet(LOCK_RANK_RATCHET_RAW))
    }

    fn is_entry_ratcheted(
        held_rank: u16,
        acquired_rank: u16,
        rel_file: &str,
        resolved_key: Option<&RatchetSiteKey>,
    ) -> bool {
        let baseline = baseline_ratchet();
        if let Some(key) = resolved_key {
            return baseline.contains(&(held_rank, acquired_rank, key.to_string()));
        }
        let prefix = format!("{rel_file}::");
        baseline
            .iter()
            .any(|(h, a, k)| *h == held_rank && *a == acquired_rank && k.starts_with(&prefix))
    }

    fn current_mode_and_path() -> (LockRankMode, Option<PathBuf>) {
        #[cfg(test)]
        if let Some(cfg) = TEST_CONFIG.with(|c| c.borrow().clone()) {
            return cfg;
        }
        let mode = match std::env::var("EVOMU_LOCK_RANK") {
            Ok(val) => match val.trim().to_ascii_lowercase().as_str() {
                "" | "default" => {
                    if baseline_ratchet().is_empty() {
                        LockRankMode::Panic
                    } else {
                        LockRankMode::EnforceRatchet
                    }
                }
                "panic" | "strict" => LockRankMode::Panic,
                "ratchet" | "enforce" => {
                    if baseline_ratchet().is_empty() {
                        LockRankMode::Panic
                    } else {
                        LockRankMode::EnforceRatchet
                    }
                }
                "record" => LockRankMode::Record,
                "warn" => LockRankMode::Warn,
                _ => LockRankMode::Panic,
            },
            Err(_) => {
                if baseline_ratchet().is_empty() {
                    LockRankMode::Panic
                } else {
                    LockRankMode::EnforceRatchet
                }
            }
        };
        let path = std::env::var_os("EVOMU_LOCK_RANK_FILE").map(PathBuf::from);
        (mode, path)
    }

    fn record_triple(triple_line: &str, override_path: Option<PathBuf>) {
        static RECORDED: OnceLock<Mutex<BTreeMap<PathBuf, BTreeSet<String>>>> = OnceLock::new();
        let target = override_path.unwrap_or_else(|| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/lock_rank_ratchet.txt"))
        });
        let map_lock = RECORDED.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut by_path = map_lock.lock().unwrap_or_else(|e| e.into_inner());
        let seen = by_path.entry(target.clone()).or_insert_with(|| {
            let mut set = BTreeSet::new();
            if let Ok(existing) = std::fs::read_to_string(&target) {
                for line in existing.lines() {
                    let t = line.trim();
                    if !t.is_empty() && !t.starts_with('#') {
                        set.insert(t.to_string());
                    }
                }
            }
            set
        });
        if seen.insert(triple_line.to_string()) {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::File::create(&target) {
                for line in seen.iter() {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
    }

    fn stack_summary(entries: &[HeldEntry]) -> String {
        entries
            .iter()
            .map(|e| format!("{}@{}", e.rank, format_location(e.site)))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    fn report_violation(
        held_rank: u16,
        held_site: &'static Location<'static>,
        acquired_rank: u16,
        acquired_site: &'static Location<'static>,
        stack: &str,
        context: &str,
    ) {
        let held_loc = format_location(held_site);
        let acq_loc = format_location(acquired_site);
        let rel_file = normalize_rel_file(acquired_site.file());
        let resolved_key = resolve_ratchet_key(acquired_site);
        let acq_key_str = resolved_key
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| acq_loc.clone());
        let (mode, record_path) = current_mode_and_path();
        match mode {
            LockRankMode::Record => {
                let triple = format!("{held_rank}\t{acquired_rank}\t{acq_key_str}");
                record_triple(&triple, record_path);
                tracing::warn!(
                    held_rank,
                    held_site = %held_loc,
                    acquired_rank,
                    acquired_site = %acq_loc,
                    ratchet_key = %acq_key_str,
                    held_stack = %stack,
                    "{context} (recorded in lock_rank_ratchet.txt)"
                );
            }
            LockRankMode::Warn => {
                if !is_entry_ratcheted(held_rank, acquired_rank, &rel_file, resolved_key.as_ref()) {
                    UNRATCHETED_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
                }
                tracing::warn!(
                    held_rank,
                    held_site = %held_loc,
                    acquired_rank,
                    acquired_site = %acq_loc,
                    ratchet_key = %acq_key_str,
                    held_stack = %stack,
                    "{context}"
                );
            }
            LockRankMode::EnforceRatchet => {
                if is_entry_ratcheted(held_rank, acquired_rank, &rel_file, resolved_key.as_ref()) {
                    tracing::warn!(
                        held_rank,
                        held_site = %held_loc,
                        acquired_rank,
                        acquired_site = %acq_loc,
                        ratchet_key = %acq_key_str,
                        "{context} (allowed by lock_rank_ratchet.txt)"
                    );
                } else {
                    UNRATCHETED_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
                    panic!(
                        "lock-rank violation ({context}): cannot acquire rank {acquired_rank} at {acq_loc} (key: {acq_key_str}) while holding rank {held_rank} at {held_loc} [held: {stack}]"
                    );
                }
            }
            LockRankMode::Panic => {
                UNRATCHETED_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
                panic!(
                    "lock-rank violation ({context}): cannot acquire rank {acquired_rank} at {acq_loc} (key: {acq_key_str}) while holding rank {held_rank} at {held_loc} [held: {stack}]"
                );
            }
        }
    }

    pub(crate) fn enter(rank: u16, site: &'static Location<'static>) -> HeldToken {
        let tid = std::thread::current().id();
        let violation = {
            let map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
            map.get(&tid).and_then(|stack| {
                stack
                    .entries
                    .iter()
                    .rev()
                    .find(|e| e.rank >= rank)
                    .copied()
                    .map(|v| (v.rank, v.site, stack_summary(&stack.entries)))
            })
        };
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(held_rank, held_site, rank, site, &summary, "out-of-order lock acquisition");
        }
        let mut map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
        let stack = map.entry(tid).or_default();
        stack.next_id = stack.next_id.saturating_add(1).max(1);
        let id = stack.next_id;
        stack.entries.push(HeldEntry { id, rank, site });
        HeldToken {
            id,
            thread_id: Some(tid),
        }
    }

    pub(crate) fn assert_no_locks_held(site: &'static Location<'static>) {
        let tid = std::thread::current().id();
        let violation = {
            let map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
            map.get(&tid).and_then(|stack| {
                stack
                    .entries
                    .last()
                    .copied()
                    .map(|top| (top.rank, top.site, stack_summary(&stack.entries)))
            })
        };
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(held_rank, held_site, 0, site, &summary, "assert_no_locks_held");
        }
    }

    pub(crate) fn assert_locks_below(max_rank: u16, site: &'static Location<'static>) {
        let tid = std::thread::current().id();
        let violation = {
            let map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
            map.get(&tid).and_then(|stack| {
                stack
                    .entries
                    .iter()
                    .rev()
                    .find(|e| e.rank >= max_rank)
                    .copied()
                    .map(|v| (v.rank, v.site, stack_summary(&stack.entries)))
            })
        };
        if let Some((held_rank, held_site, summary)) = violation {
            report_violation(
                held_rank,
                held_site,
                max_rank,
                site,
                &summary,
                "assert_locks_below",
            );
        }
    }

    pub(crate) fn held_ranks() -> Vec<u16> {
        let tid = std::thread::current().id();
        let map = held_stacks().lock().unwrap_or_else(|e| e.into_inner());
        map.get(&tid)
            .map(|stack| stack.entries.iter().map(|e| e.rank).collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn with_test_mode<R>(
        mode: LockRankMode,
        record_path: Option<PathBuf>,
        f: impl FnOnce() -> R,
    ) -> R {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                let _ = TEST_CONFIG.try_with(|c| {
                    *c.borrow_mut() = None;
                });
            }
        }
        TEST_CONFIG.with(|c| {
            *c.borrow_mut() = Some((mode, record_path));
        });
        let _reset = Reset;
        f()
    }
}

/// Asserts that the current thread holds zero ranked locks.
///
/// Called before actor `ask`/`tell` channel sends and `.await` boundaries (`L-2`).
#[inline]
#[track_caller]
pub fn assert_no_locks_held() {
    imp::assert_no_locks_held(Location::caller());
}

/// Asserts that every lock currently held on this thread has rank strictly below `max_rank`.
#[inline]
#[track_caller]
pub fn assert_locks_below(max_rank: u16) {
    imp::assert_locks_below(max_rank, Location::caller());
}

/// Returns the list of lock ranks currently held on this thread (diagnostic/test helper).
pub fn held_lock_ranks() -> Vec<u16> {
    imp::held_ranks()
}

/// Lock wrapper associating a compile-time `RANK` with an underlying synchronization primitive `L`.
#[derive(Debug)]
pub struct Ranked<L: ?Sized, const RANK: u16>(L);

impl<L: Default, const RANK: u16> Default for Ranked<L, RANK> {
    fn default() -> Self {
        Self(L::default())
    }
}

/// Type alias for a rank-checked [`std::sync::RwLock`].
pub type RankedRwLock<T, const RANK: u16> = Ranked<RwLock<T>, RANK>;

/// Type alias for a rank-checked [`std::sync::Mutex`].
pub type RankedMutex<T, const RANK: u16> = Ranked<Mutex<T>, RANK>;

impl<L: ?Sized, const RANK: u16> Ranked<L, RANK> {
    /// Returns the static lock rank associated with this lock.
    pub const fn rank(&self) -> u16 {
        RANK
    }
}

impl<L, const RANK: u16> Ranked<L, RANK> {
    /// Wraps an already-constructed lock primitive with rank `RANK`.
    pub const fn from_lock(lock: L) -> Self {
        Self(lock)
    }

    /// Consumes the wrapper and returns the underlying lock primitive.
    pub fn into_inner_lock(self) -> L {
        self.0
    }
}

/// Guard returned by [`RankedRwLock::read`]. Releases the read lock before popping the rank stack.
pub struct RankedRwLockReadGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::RwLockReadGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedRwLockReadGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedRwLockReadGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

/// Guard returned by [`RankedRwLock::write`]. Releases the write lock before popping the rank stack.
pub struct RankedRwLockWriteGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::RwLockWriteGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedRwLockWriteGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized, const RANK: u16> DerefMut for RankedRwLockWriteGuard<'_, T, RANK> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedRwLockWriteGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

/// Guard returned by [`RankedMutex::lock`]. Releases the mutex before popping the rank stack.
pub struct RankedMutexGuard<'a, T: ?Sized, const RANK: u16> {
    guard: std::sync::MutexGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized, const RANK: u16> Deref for RankedMutexGuard<'_, T, RANK> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized, const RANK: u16> DerefMut for RankedMutexGuard<'_, T, RANK> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: ?Sized + std::fmt::Debug, const RANK: u16> std::fmt::Debug
    for RankedMutexGuard<'_, T, RANK>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}

fn map_lock_result<G, W>(
    res: LockResult<G>,
    held: imp::HeldToken,
    wrap: impl FnOnce(G, imp::HeldToken) -> W,
) -> LockResult<W> {
    match res {
        Ok(guard) => Ok(wrap(guard, held)),
        Err(poison) => Err(PoisonError::new(wrap(poison.into_inner(), held))),
    }
}

fn map_try_lock_result<G, W>(
    res: TryLockResult<G>,
    rank: u16,
    site: &'static Location<'static>,
    wrap: impl FnOnce(G, imp::HeldToken) -> W,
) -> TryLockResult<W> {
    match res {
        Ok(guard) => {
            let held = imp::enter(rank, site);
            Ok(wrap(guard, held))
        }
        Err(TryLockError::Poisoned(poison)) => {
            let held = imp::enter(rank, site);
            Err(TryLockError::Poisoned(PoisonError::new(wrap(
                poison.into_inner(),
                held,
            ))))
        }
        Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
    }
}

impl<T, const RANK: u16> Ranked<RwLock<T>, RANK> {
    /// Creates a new rank-checked `RwLock` wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    /// Consumes this lock, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        self.0.into_inner()
    }
}

impl<T: ?Sized, const RANK: u16> Ranked<RwLock<T>, RANK> {
    /// Acquires shared read access, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn read(&self) -> LockResult<RankedRwLockReadGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.read(), held, |guard, _held| RankedRwLockReadGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire shared read access without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_read(&self) -> TryLockResult<RankedRwLockReadGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_read(), RANK, site, |guard, _held| {
            RankedRwLockReadGuard { guard, _held }
        })
    }

    /// Acquires exclusive write access, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn write(&self) -> LockResult<RankedRwLockWriteGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.write(), held, |guard, _held| RankedRwLockWriteGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire exclusive write access without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_write(&self) -> TryLockResult<RankedRwLockWriteGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_write(), RANK, site, |guard, _held| {
            RankedRwLockWriteGuard { guard, _held }
        })
    }

    /// Acquires shared read access on the underlying `RwLock` without pushing `RANK` onto the
    /// thread's held-rank stack (used only for brief clone-and-drop leaf reads of handle tables).
    pub fn read_unranked(&self) -> LockResult<std::sync::RwLockReadGuard<'_, T>> {
        self.0.read()
    }

    /// Attempts non-blocking exclusive write access on the underlying `RwLock` without pushing
    /// `RANK` onto the thread's held-rank stack (used only with an explicit rank bound check for
    /// cross-system non-blocking cleanup).
    pub fn try_write_unranked(&self) -> TryLockResult<std::sync::RwLockWriteGuard<'_, T>> {
        self.0.try_write()
    }

    /// Returns whether the underlying `RwLock` is currently poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.0.is_poisoned()
    }

    /// Returns a mutable reference to the underlying data with exclusive borrow access.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.0.get_mut()
    }
}

impl<T, const RANK: u16> Ranked<Mutex<T>, RANK> {
    /// Creates a new rank-checked `Mutex` wrapping `value`.
    pub const fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    /// Consumes this lock, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        self.0.into_inner()
    }
}

impl<T: ?Sized, const RANK: u16> Ranked<Mutex<T>, RANK> {
    /// Acquires the mutex, enforcing `RANK` against locks currently held on this thread.
    #[track_caller]
    pub fn lock(&self) -> LockResult<RankedMutexGuard<'_, T, RANK>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.lock(), held, |guard, _held| RankedMutexGuard {
            guard,
            _held,
        })
    }

    /// Attempts to acquire the mutex without blocking, enforcing `RANK` when acquired.
    #[track_caller]
    pub fn try_lock(&self) -> TryLockResult<RankedMutexGuard<'_, T, RANK>> {
        let site = Location::caller();
        map_try_lock_result(self.0.try_lock(), RANK, site, |guard, _held| RankedMutexGuard {
            guard,
            _held,
        })
    }

    /// Returns whether the underlying `Mutex` is currently poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.0.is_poisoned()
    }

    /// Returns a mutable reference to the underlying data with exclusive borrow access.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.0.get_mut()
    }
}

/// Rank-erased shared read guard returned by [`AnyRwLock::read`].
pub struct AnyRwLockReadGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockReadGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized> Deref for AnyRwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

/// Rank-erased exclusive write guard returned by [`AnyRwLock::write`].
pub struct AnyRwLockWriteGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockWriteGuard<'a, T>,
    _held: imp::HeldToken,
}

impl<T: ?Sized> Deref for AnyRwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T: ?Sized> DerefMut for AnyRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

/// Object-safe adapter allowing helper functions to accept either a [`RankedRwLock`]
/// (enforcing its static rank) or a raw [`std::sync::RwLock`] (e.g. local unit-test fixtures).
pub trait AnyRwLock<T: ?Sized>: Send + Sync {
    /// Acquires shared read access.
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>>;
    /// Acquires exclusive write access.
    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>>;
}

impl<T: ?Sized + Send + Sync> AnyRwLock<T> for RwLock<T> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        map_lock_result(RwLock::read(self), imp::HeldToken::none(), |guard, _held| {
            AnyRwLockReadGuard { guard, _held }
        })
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        map_lock_result(RwLock::write(self), imp::HeldToken::none(), |guard, _held| {
            AnyRwLockWriteGuard { guard, _held }
        })
    }
}

impl<T: ?Sized + Send + Sync> AnyRwLock<T> for std::sync::Arc<RwLock<T>> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        <RwLock<T> as AnyRwLock<T>>::read(&**self)
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        <RwLock<T> as AnyRwLock<T>>::write(&**self)
    }
}

impl<T: ?Sized + Send + Sync, const RANK: u16> AnyRwLock<T> for Ranked<RwLock<T>, RANK> {
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.read(), held, |guard, _held| AnyRwLockReadGuard {
            guard,
            _held,
        })
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        let held = imp::enter(RANK, Location::caller());
        map_lock_result(self.0.write(), held, |guard, _held| AnyRwLockWriteGuard {
            guard,
            _held,
        })
    }
}

impl<T: ?Sized + Send + Sync, const RANK: u16> AnyRwLock<T>
    for std::sync::Arc<Ranked<RwLock<T>, RANK>>
{
    #[track_caller]
    fn read(&self) -> LockResult<AnyRwLockReadGuard<'_, T>> {
        <Ranked<RwLock<T>, RANK> as AnyRwLock<T>>::read(&**self)
    }

    #[track_caller]
    fn write(&self) -> LockResult<AnyRwLockWriteGuard<'_, T>> {
        <Ranked<RwLock<T>, RANK> as AnyRwLock<T>>::write(&**self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    fn temp_ratchet_path(name: &str) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "evomu_lock_rank_{name}_{}_{seq}.txt",
            std::process::id()
        ))
    }

    #[test]
    fn strictly_ascending_acquisition_succeeds_and_drops_cleanly() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(1);
            let actor: RankedRwLock<u32, RANK_ACTOR_STATE> = RankedRwLock::new(2);
            let cw: RankedRwLock<u32, RANK_CRIMEWATCH> = RankedRwLock::new(3);
            let inner: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(4);
            let conn: RankedMutex<u32, RANK_SQLITE_CONN> = RankedMutex::new(5);
            let fp: RankedMutex<u32, RANK_SQLITE_FP> = RankedMutex::new(6);
            let wal: RankedMutex<u32, RANK_WAL_LOCK> = RankedMutex::new(7);
            let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(8);

            assert!(held_lock_ranks().is_empty());
            assert_no_locks_held();
            {
                let _g20 = bp.read().unwrap();
                let _g21 = actor.write().unwrap();
                let _g30 = cw.read().unwrap();
                let _g40 = inner.write().unwrap();
                let _g41 = conn.lock().unwrap();
                let _g42 = fp.lock().unwrap();
                let _g43 = wal.lock().unwrap();
                let _g60 = leaf.read().unwrap();
                assert_eq!(held_lock_ranks(), vec![20, 21, 30, 40, 41, 42, 43, 60]);
            }
            assert!(held_lock_ranks().is_empty());
            assert_no_locks_held();

            // Re-acquiring lowest rank after drop succeeds.
            let _g20_again = bp.write().unwrap();
            assert_eq!(held_lock_ranks(), vec![20]);
        });
    }

    #[test]
    fn wrong_order_reports_both_call_sites() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(10);
            let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(20);

            let outer_line = line!() + 1;
            let _outer = leaf.read().unwrap();
            let inner_line = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _inner = world.write();
            }))
            .expect_err("acquiring rank 40 while holding rank 60 must panic");

            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&'static str>().map(|s| (*s).to_string()))
                .expect("panic payload must be string");
            assert!(msg.contains("rank 40"), "unexpected msg: {msg}");
            assert!(msg.contains("rank 60"), "unexpected msg: {msg}");
            assert!(
                msg.contains(&format!(":{outer_line}:")),
                "missing outer call site line {outer_line} in: {msg}"
            );
            assert!(
                msg.contains(&format!(":{inner_line}:")),
                "missing inner call site line {inner_line} in: {msg}"
            );
        });
        assert!(held_lock_ranks().is_empty());
    }

    #[test]
    fn same_rank_acquisition_panics_with_both_sites() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let a: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(1);
            let b: RankedMutex<u32, RANK_LEAF> = RankedMutex::new(2);

            let line_a = line!() + 1;
            let _ga = a.read().unwrap();
            let line_b = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _gb = b.lock();
            }))
            .expect_err("two locks of the same rank must fail");

            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            assert!(msg.contains(&format!(":{line_a}:")), "msg: {msg}");
            assert!(msg.contains(&format!(":{line_b}:")), "msg: {msg}");
        });
        assert!(held_lock_ranks().is_empty());
    }

    #[test]
    fn assert_no_locks_held_reports_held_site_and_caller_site() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let held_line = line!() + 1;
            let _g = bp.read().unwrap();
            let assert_line = line!() + 2;
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_no_locks_held();
            }))
            .expect_err("assert_no_locks_held must fail while lock is held");
            let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(msg.contains("assert_no_locks_held"), "msg: {msg}");
            assert!(msg.contains(&format!(":{held_line}:")), "msg: {msg}");
            assert!(msg.contains(&format!(":{assert_line}:")), "msg: {msg}");
        });
        assert_no_locks_held();
    }

    #[test]
    fn out_of_lifo_drop_keeps_stack_consistent_and_try_would_block_pops() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let l20: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let l40: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(0);
            let l30: RankedRwLock<u32, RANK_CRIMEWATCH> = RankedRwLock::new(0);
            let l60 = std::sync::Arc::new(RankedMutex::<u32, RANK_LEAF>::new(0));

            let g20 = l20.read().unwrap();
            let g40 = l40.read().unwrap();
            drop(g20);
            assert_eq!(held_lock_ranks(), vec![40]);

            // Holding 40 still forbids 30.
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _g30 = l30.read();
            }));
            assert!(res.is_err());
            assert_eq!(held_lock_ranks(), vec![40]);

            // WouldBlock on try_lock must not leave rank 60 on stack.
            let l60_bg = std::sync::Arc::clone(&l60);
            let (tx, rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let _held = l60_bg.lock().unwrap();
                tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            rx.recv().unwrap();
            assert!(matches!(l60.try_lock(), Err(TryLockError::WouldBlock)));
            assert_eq!(held_lock_ranks(), vec![40]);
            release_tx.send(()).unwrap();
            handle.join().unwrap();

            drop(g40);
            assert!(held_lock_ranks().is_empty());
        });
    }

    #[test]
    fn poison_recovery_preserves_balanced_stack() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let l60 = std::sync::Arc::new(RankedMutex::<u32, RANK_LEAF>::new(1));
            let l60_bg = std::sync::Arc::clone(&l60);
            let _ = std::thread::spawn(move || {
                let _g = l60_bg.lock().unwrap();
                panic!("poison mutex intentionally");
            })
            .join();
            {
                let guard = l60.lock().unwrap_or_else(|e| e.into_inner());
                assert_eq!(*guard, 1);
                assert_eq!(held_lock_ranks(), vec![60]);
            }
            assert!(held_lock_ranks().is_empty());
        });
    }

    #[test]
    fn committed_ratchet_policy_and_record_enforce_roundtrip() {
        let non_comment_lines = LOCK_RANK_RATCHET_RAW
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .count();
        let parsed = imp::parse_ratchet(LOCK_RANK_RATCHET_RAW);
        assert_eq!(
            parsed.len(),
            non_comment_lines,
            "every non-comment line in crates/eve-db/lock_rank_ratchet.txt must parse as a unique structured triple"
        );
        let validated_count = validate_ratchet_against_workspace(LOCK_RANK_RATCHET_RAW, &workspace_root())
            .expect("committed lock_rank_ratchet.txt must validate against live workspace sites with 0 stale entries");
        assert_eq!(validated_count, non_comment_lines);
        assert!(
            parsed
                .iter()
                .all(|(h, a, _)| !(*h == RANK_BALLPARK && *a == RANK_BALLPARK)),
            "committed ratchet must never allow same-rank Ballpark (20 -> 20) re-entry"
        );
        assert!(
            parsed.iter().all(|(h, _, _)| *h != RANK_SQLITE_FP && *h != RANK_SQLITE_CONN),
            "committed ratchet must never allow stale RANK_SQLITE_CONN/FP (41/42) entries"
        );
        assert!(
            parsed.iter().all(|(h, a, _)| {
                !(*h == RANK_LEAF && (*a == RANK_BALLPARK || *a == RANK_ACTOR_STATE))
            }),
            "committed ratchet must never allow RANK_LEAF (60) -> Ballpark (20) / ActorState (21) inversions"
        );

        let path = temp_ratchet_path("roundtrip");
        let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(1);
        let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(2);

        imp::with_test_mode(LockRankMode::Record, Some(path.clone()), || {
            let _outer = leaf.read().unwrap();
            let _inner = world.read().unwrap();
        });
        let recorded = std::fs::read_to_string(&path).expect("ratchet file created");
        let entries = imp::parse_ratchet(&recorded);
        assert_eq!(entries.len(), 1, "expected 1 recorded triple, got: {recorded}");
        let (h, a, site) = entries.clone().into_iter().next().unwrap();
        assert_eq!((h, a), (RANK_LEAF, RANK_WORLD_INNER));
        assert!(site.contains("lock_rank.rs"), "site: {site}");
        let key = RatchetSiteKey::parse(&site)
            .expect("recorded ratchet entry must use structured file::fn::field key");
        assert_eq!(
            key.enclosing_fn,
            "committed_ratchet_policy_and_record_enforce_roundtrip"
        );
        assert_eq!(key.field_name, "world");
        assert!(!entries.contains(&(RANK_LEAF, RANK_BALLPARK, site)));

        // Re-recording the same site is deduplicated; assert_locks_below emits distinct field key.
        imp::with_test_mode(LockRankMode::Record, Some(path.clone()), || {
            let _g = leaf.read().unwrap();
            assert_locks_below(RANK_WORLD_INNER);
        });
        let recorded_after = std::fs::read_to_string(&path).expect("ratchet file exists");
        assert_eq!(imp::parse_ratchet(&recorded_after).len(), 2);

        imp::with_test_mode(LockRankMode::Panic, None, || {
            let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(0);
            let _g20 = bp.read().unwrap();
            assert_locks_below(RANK_WORLD_INNER);
            let _g40 = world.read().unwrap();
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_locks_below(RANK_WORLD_INNER);
            }));
            assert!(err.is_err());
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ratchet_validation_rejects_stale_and_line_keyed_entries() {
        let root = workspace_root();

        let stale = "60\t40\tcrates/eve-db/src/lock_rank.rs::nonexistent_ratchet_fn::world\n";
        let err = validate_ratchet_against_workspace(stale, &root)
            .expect_err("stale entry matching 0 live sites must fail loudly");
        assert!(
            err.iter().any(|m| m.contains("matches 0 live lock sites")),
            "unexpected stale error: {err:?}"
        );

        let legacy_line_col = "60\t40\tcrates/eve-db/src/lock_rank.rs:1020:15\n";
        let legacy_err = parse_ratchet_entries(legacy_line_col)
            .expect_err("legacy file:line:col key must be rejected");
        assert!(
            legacy_err.contains("file:line:col"),
            "unexpected legacy error: {legacy_err}"
        );
        assert!(validate_ratchet_against_workspace(legacy_line_col, &root).is_err());

        let dup = "60\t40\tcrates/eve-db/src/inventory.rs::get_item::read_state\n\
                   60\t40\tcrates/eve-db/src/inventory.rs::get_item::read_state\n";
        assert!(
            validate_ratchet_against_workspace(dup, &root).is_err(),
            "duplicate structured ratchet entries must fail loudly"
        );
    }

    #[test]
    fn enforce_ratchet_tracks_unconditional_held_locks_and_counts_violations() {
        // Default mode (no test override) still tracks held_lock_ranks identically
        // across debug/test and release builds.
        let bp: RankedRwLock<u32, RANK_BALLPARK> = RankedRwLock::new(1);
        let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(2);
        let leaf: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(3);
        assert!(held_lock_ranks().is_empty());
        {
            let _g20 = bp.read().unwrap();
            assert_eq!(held_lock_ranks(), vec![RANK_BALLPARK]);
            let _g40 = world.read().unwrap();
            assert_eq!(held_lock_ranks(), vec![RANK_BALLPARK, RANK_WORLD_INNER]);
        }
        assert!(held_lock_ranks().is_empty());

        // Unlisted inversion under EnforceRatchet must panic AND increment
        // unratcheted_violation_count so catch_unwind cannot hide it.
        let before = unratcheted_violation_count();
        imp::with_test_mode(LockRankMode::EnforceRatchet, None, || {
            let _g60 = leaf.read().unwrap();
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _unlisted = world.read();
            }));
            assert!(err.is_err(), "unlisted 60->40 must panic in EnforceRatchet");
        });
        assert!(
            unratcheted_violation_count() > before,
            "unratcheted_violation_count must increment before panicking"
        );
        assert!(held_lock_ranks().is_empty());
    }

    #[test]
    fn any_rwlock_enforces_rank_for_ranked_and_allows_raw() {
        imp::with_test_mode(LockRankMode::Panic, None, || {
            let ranked: RankedRwLock<u32, RANK_LEAF> = RankedRwLock::new(42);
            let world: RankedRwLock<u32, RANK_WORLD_INNER> = RankedRwLock::new(7);
            let raw: RwLock<u32> = RwLock::new(99);

            fn lock_via_trait(r: &dyn AnyRwLock<u32>) -> u32 {
                *r.read().unwrap()
            }
            assert_eq!(lock_via_trait(&ranked), 42);
            assert_eq!(lock_via_trait(&raw), 99);

            let _g = (&ranked as &dyn AnyRwLock<u32>).read().unwrap();
            assert_eq!(held_lock_ranks(), vec![RANK_LEAF]);
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _w = world.read();
            }));
            assert!(err.is_err());
        });
    }
}
