//! Table-driven negative-test security matrix exercising every registered `(service, method)`
//! and bound-object method across six caller/world situations via the client wire path (`#1343`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use eve_db::durable_domain_types::{
    PersistedAggressionRecord, PersistedCombatCrimewatchState, PersistedCrimeWatchState,
};
use eve_db::snapshot_ext::WorldStateSnapshot;
use eve_db::types::{FleetMemberRecord, FleetRole, KillmailRecord};
use eve_db::{
    service_roles, AllianceRecord, BookmarkRecord, CharacterRecord, CorpProjectRecord,
    ContractRecord, CorpGovState, CorporationRecord, FleetRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MailRecord, MarketOrderRecord, PlanetaryColonyRecord,
    SavedFittingRecord, TrainedSkillRecord, UpwellStructureRecord, WarRecord, WorldRepository,
    WorldTablesSnapshot,
};
use eve_destiny::{Ball, BallId, CombatState, TargetLock, WarpBubbleMgr};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use eve_server::esi::runtime_state::{
    EsiCalendarEvent, EsiContactEntry, EsiContactNotification, EsiCorpHistoryEntry, EsiMailLabel,
    EsiMailMeta, EsiRuntimeState,
};
use eve_server::router::{BoundObjectEntry, ServerContext};
use eve_server::services::industry_pi::industry_facilities_bps::{
    save_job_cost_meta, IndustryJobCostMeta,
};
use eve_testkit::client_harness::ContractValidationMode;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common;

const SYS_JITA: SolarSystemId = 30_000_142;
const PREFERRED_OTHER_SYS: SolarSystemId = 30_000_144;
const NPC_STATION_ID: i64 = 60_003_760;

const USER_A_ID: u64 = 990_001;
const USER_B_ID: u64 = 990_002;
const USER_C_ID: u64 = 990_003;
const USER_D_ID: u64 = 990_004;
const USER_E_ID: u64 = 990_005;
const USER_F_ID: u64 = 990_006;
const USER_A_CEO_ID: u64 = 990_007;

const CHAR_A_ID: i64 = 2_199_001;
const CHAR_B_ID: i64 = 2_199_002;
const CHAR_C_ID: i64 = 2_199_003;
const CHAR_D_ID: i64 = 2_199_004;
const CHAR_E_ID: i64 = 2_199_005;
const CHAR_F_ID: i64 = 2_199_006;
const CHAR_A_CEO_ID: i64 = 2_199_007;

const CORP_A_ID: i64 = 98_099_001;
const CORP_B_ID: i64 = 98_099_002;
const CORP_D_ID: i64 = 98_099_004;
const CORP_E_ID: i64 = 98_099_005;
const CORP_F_ID: i64 = 98_099_006;
const ALLIANCE_B_ID: i64 = 99_099_002;

const CORP_A_OFFICE_ID: i64 = 9_199_001;
const CORP_A_OFFICE_ITEM_ID: i64 = 9_199_002;
const CORP_A_STRUCTURE_ID: i64 = 1_099_001_000_001;
const CORP_B_OFFICE_ID: i64 = 9_199_011;
const CORP_B_OFFICE_ITEM_ID: i64 = 9_199_012;
const CORP_B_STRUCTURE_ID: i64 = 1_099_002_000_001;
const CORP_D_OFFICE_ID: i64 = 9_199_041;
const CORP_D_OFFICE_ITEM_ID: i64 = 9_199_042;
const CORP_D_STRUCTURE_ID: i64 = 1_099_004_000_001;
const CORP_E_OFFICE_ID: i64 = 9_199_051;
const CORP_E_OFFICE_ITEM_ID: i64 = 9_199_052;
const CORP_E_STRUCTURE_ID: i64 = 1_099_005_000_001;
const CORP_F_OFFICE_ID: i64 = 9_199_061;
const CORP_F_OFFICE_ITEM_ID: i64 = 9_199_062;
const CORP_F_STRUCTURE_ID: i64 = 1_099_006_000_001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Situation {
    A,
    B,
    C,
    D,
    E,
    F,
}

impl Situation {
    const ALL: [Situation; 6] = [
        Situation::A,
        Situation::B,
        Situation::C,
        Situation::D,
        Situation::E,
        Situation::F,
    ];

    fn code(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct MatrixViolation {
    situation: String,
    service: String,
    method: String,
    kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ViolationGroup {
    situation: String,
    service: String,
    kind: String,
    methods: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ViolationsRatchetFile {
    version: u32,
    description: String,
    total_violations: usize,
    violation_groups: Vec<ViolationGroup>,
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        static DIR_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}_{}_{}_{}",
            std::process::id(),
            seq,
            nanos
        ));
        let _ = std::fs::create_dir_all(&path);
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root two levels above eve-server Cargo.toml")
        .to_path_buf()
}

// ============================================================================
// Pure-Rust Authoritative RPC Discovery & Ratchet Loading
// ============================================================================

fn strip_comments_preserving_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        let c = bytes[i];
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i < n && depth > 0 {
                if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                    depth += 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                    depth -= 1;
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                } else {
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
        } else if (i == 0
            || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
            && c == b'r'
            && i + 1 < n
            && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#')
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && bytes[j] == b'"' {
                let mut term = Vec::with_capacity(1 + hashes);
                term.push(b'"');
                term.resize(1 + hashes, b'#');
                if let Some(pos) = find_bytes(bytes, &term, j + 1) {
                    i = pos + term.len();
                } else {
                    i = n;
                }
            } else {
                i += 1;
            }
        } else if c == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if c == b'\'' {
            if i + 2 < n && bytes[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < n && j < i + 8 && bytes[j] != b'\'' {
                    j += 1;
                }
                if j < n && bytes[j] == b'\'' {
                    i = j + 1;
                    continue;
                }
            } else if i + 2 < n && bytes[i + 2] == b'\'' {
                i += 3;
                continue;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|rel| from + rel)
}

fn skip_string_or_char(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    let c = bytes[i];
    if c == b'\\' {
        return None;
    }
    if c == b'"' {
        let mut j = i + 1;
        while j < n {
            if bytes[j] == b'\\' {
                j += 2;
            } else if bytes[j] == b'"' {
                return Some(j + 1);
            } else {
                j += 1;
            }
        }
        return Some(n);
    }
    let ident_boundary =
        i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
    if ident_boundary && c == b'r' && i + 1 < n && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
        let mut j = i + 1;
        let mut hashes = 0usize;
        while j < n && bytes[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if j < n && bytes[j] == b'"' {
            let mut term = Vec::with_capacity(1 + hashes);
            term.push(b'"');
            term.resize(1 + hashes, b'#');
            return Some(find_bytes(bytes, &term, j + 1).map_or(n, |p| p + term.len()));
        }
    }
    if c == b'\'' {
        if i + 2 < n && bytes[i + 1] == b'\\' {
            let mut j = i + 2;
            while j < n && j < i + 8 && bytes[j] != b'\'' {
                j += 1;
            }
            if j < n && bytes[j] == b'\'' {
                return Some(j + 1);
            }
        } else if i + 2 < n && bytes[i + 2] == b'\'' {
            return Some(i + 3);
        }
    }
    None
}

fn brace_block(code: &str, start_after_open_brace: usize) -> (&str, usize) {
    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut depth = 1usize;
    let mut i = start_after_open_brace;
    while i < n && depth > 0 {
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    let end = i.saturating_sub(1).max(start_after_open_brace);
    (&code[start_after_open_brace..end], i)
}

fn strip_cfg_test_items(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = bytes.to_vec();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if !code.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        if bytes[i] == b'#' {
            let mut j = i + 1;
            while j < n && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && bytes[j] == b'[' {
                let mut k = j + 1;
                while k < n && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if code[k..].starts_with("cfg") {
                    let mut p = k + 3;
                    while p < n && bytes[p].is_ascii_whitespace() {
                        p += 1;
                    }
                    if p < n && bytes[p] == b'(' {
                        p += 1;
                        while p < n && bytes[p].is_ascii_whitespace() {
                            p += 1;
                        }
                        if code[p..].starts_with("test") {
                            p += 4;
                            while p < n && bytes[p].is_ascii_whitespace() {
                                p += 1;
                            }
                            if p < n && bytes[p] == b')' {
                                p += 1;
                                while p < n && bytes[p].is_ascii_whitespace() {
                                    p += 1;
                                }
                                if p < n && bytes[p] == b']' {
                                    let mut scan = p + 1;
                                    let mut d_par = 0usize;
                                    let mut end_idx = None;
                                    while scan < n {
                                        if let Some(next_s) = skip_string_or_char(bytes, scan) {
                                            scan = next_s;
                                            continue;
                                        }
                                        match bytes[scan] {
                                            b'(' | b'[' => d_par += 1,
                                            b')' | b']' => d_par = d_par.saturating_sub(1),
                                            b'{' if d_par == 0 => {
                                                let (_, after) = brace_block(code, scan + 1);
                                                end_idx = Some(after);
                                                break;
                                            }
                                            b';' if d_par == 0 => {
                                                end_idx = Some(scan + 1);
                                                break;
                                            }
                                            _ => {}
                                        }
                                        scan += 1;
                                    }
                                    if let Some(end) = end_idx {
                                        for slot in &mut out[i..end] {
                                            if *slot != b'\n' {
                                                *slot = b' ';
                                            }
                                        }
                                        i = end;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_top_arms(block: &str) -> Vec<(&str, &str)> {
    let bytes = block.as_bytes();
    let n = bytes.len();
    let mut arms = Vec::new();
    let mut d = 0isize;
    let mut arm_start = 0usize;
    let mut i = 0usize;
    while i < n {
        if !block.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        match bytes[i] {
            b'{' | b'(' | b'[' => d += 1,
            b'}' | b')' | b']' => d -= 1,
            b'=' if d == 0 && i + 1 < n && bytes[i + 1] == b'>' => {
                let lhs = &block[arm_start..i];
                let mut j = i + 2;
                while j < n && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let body;
                if j < n && bytes[j] == b'{' {
                    let (b, end_pos) = brace_block(block, j + 1);
                    body = b;
                    j = end_pos;
                    while j < n && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < n && bytes[j] == b',' {
                        j += 1;
                    }
                } else {
                    let body_start = j;
                    let mut bd = 0isize;
                    while j < n {
                        if let Some(next_j) = skip_string_or_char(bytes, j) {
                            j = next_j;
                            continue;
                        }
                        match bytes[j] {
                            b'{' | b'(' | b'[' => bd += 1,
                            b'}' | b')' | b']' => bd -= 1,
                            b',' if bd == 0 => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    body = &block[body_start..j];
                    if j < n && bytes[j] == b',' {
                        j += 1;
                    }
                }
                arms.push((lhs, body));
                arm_start = j;
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    arms
}

fn split_guard(pat: &str) -> (&str, &str) {
    let bytes = pat.as_bytes();
    let n = bytes.len();
    let mut d = 0isize;
    let mut i = 0usize;
    while i + 3 < n {
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        match bytes[i] {
            b'(' | b'{' | b'[' => d += 1,
            b')' | b'}' | b']' => d -= 1,
            b' ' | b'\n' if d == 0 => {
                if bytes[i + 1..].starts_with(b"if") {
                    let after_if = i + 3;
                    if after_if < n
                        && (bytes[after_if] == b' '
                            || bytes[after_if] == b'('
                            || bytes[after_if] == b'\n')
                    {
                        return (&pat[..i], &pat[i..]);
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    (pat, "")
}

fn extract_ident_strings(s: &str) -> Vec<String> {
    extract_ident_strings_with_consts(s, &HashMap::new())
}

fn extract_ident_strings_with_consts(s: &str, consts: &HashMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            let mut valid = true;
            while j < n && bytes[j] != b'"' {
                let c = bytes[j];
                if !(c.is_ascii_alphanumeric() || c == b'_') {
                    valid = false;
                }
                if c == b'\\' {
                    j += 2;
                    valid = false;
                    continue;
                }
                j += 1;
            }
            if valid && j < n && j > start {
                out.push(s[start..j].to_string());
            }
            i = j + 1;
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            let mut j = i + 1;
            while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if let Some(val) = consts.get(&s[start..j]) {
                out.push(val.clone());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

fn extract_file_str_consts(code: &str) -> HashMap<String, String> {
    let mut consts = HashMap::new();
    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if !code.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        let ok_boundary =
            i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if ok_boundary && code[i..].starts_with("const") {
            let mut p = i + "const".len();
            if p < n && bytes[p].is_ascii_whitespace() {
                while p < n && bytes[p].is_ascii_whitespace() {
                    p += 1;
                }
                if p < n && (bytes[p].is_ascii_alphabetic() || bytes[p] == b'_') {
                    let id_start = p;
                    while p < n && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_') {
                        p += 1;
                    }
                    let ident = &code[id_start..p];
                    if !matches!(ident, "fn" | "unsafe" | "extern") {
                        while p < n && bytes[p].is_ascii_whitespace() {
                            p += 1;
                        }
                        if p < n && bytes[p] == b':' {
                            let ty_start = p + 1;
                            p += 1;
                            while p < n
                                && !matches!(bytes[p], b'=' | b';' | b'{' | b'}')
                            {
                                if let Some(nx) = skip_string_or_char(bytes, p) {
                                    p = nx;
                                    continue;
                                }
                                p += 1;
                            }
                            if p < n && bytes[p] == b'=' {
                                let ty_slice = code[ty_start..p].trim();
                                let is_str_ty = ty_slice.starts_with('&') && ty_slice.ends_with("str");
                                if is_str_ty {
                                    let rhs = code[p + 1..].trim_start();
                                    if let Some(inner) = rhs.strip_prefix('"') {
                                        if let Some(end) = inner.find('"') {
                                            let val = &inner[..end];
                                            let after_val = inner[end + 1..].trim_start();
                                            if after_val.starts_with(';')
                                                && !val.is_empty()
                                                && val.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                                            {
                                                consts.insert(ident.to_string(), val.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
    consts
}

fn expand_tuple_or_pattern(pat: &str, consts: &HashMap<String, String>) -> Vec<(String, String)> {
    let bytes = pat.as_bytes();
    let n = bytes.len();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < n {
        if !pat.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        if bytes[i] == b'(' {
            let mut d = 1usize;
            let mut j = i + 1;
            let mut top_comma = None;
            while j < n && d > 0 {
                if let Some(next_j) = skip_string_or_char(bytes, j) {
                    j = next_j;
                    continue;
                }
                match bytes[j] {
                    b'(' | b'{' | b'[' => d += 1,
                    b')' | b'}' | b']' => d -= 1,
                    b',' if d == 1 && top_comma.is_none() => top_comma = Some(j),
                    _ => {}
                }
                j += 1;
            }
            if d == 0 {
                if let Some(comma) = top_comma {
                    let left = &pat[i + 1..comma];
                    let mut right = pat[comma + 1..j - 1].trim();
                    if let Some(stripped) = right.strip_suffix(',') {
                        right = stripped;
                    }
                    let svcs = extract_ident_strings_with_consts(left, consts);
                    let meths = extract_ident_strings_with_consts(right, consts);
                    if !svcs.is_empty() && !meths.is_empty() {
                        for s in &svcs {
                            for m in &meths {
                                pairs.push((s.clone(), m.clone()));
                            }
                        }
                    } else if left.trim() == "_" && !meths.is_empty() {
                        for m in meths {
                            pairs.push(("_".to_string(), m));
                        }
                    }
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    pairs
}

fn parse_icase_guard_pair(lhs: &str) -> Option<(String, String)> {
    let idx = lhs.find(".eq_ignore_ascii_case(")?;
    let prefix = lhs[..idx].trim_end();
    let var_name = prefix
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()?;
    if var_name.is_empty() {
        return None;
    }
    let after_open = &lhs[idx + ".eq_ignore_ascii_case(".len()..];
    let meth = extract_ident_strings(after_open).into_iter().next()?;
    let pat_part = split_guard(lhs).0;
    let svc = extract_ident_strings(pat_part).into_iter().next()?;
    Some((svc, meth))
}

fn extract_bindable_services(src_root: &Path) -> BTreeSet<String> {
    let raw = std::fs::read_to_string(src_root.join("router/binding_auth.rs"))
        .expect("read binding_auth.rs");
    let code = strip_cfg_test_items(&strip_comments_preserving_strings(&raw));
    let fn_pos = code
        .find("fn authorize_service_binding")
        .expect("authorize_service_binding present");
    let match_rel = code[fn_pos..]
        .find("match service {")
        .expect("match service in authorize_service_binding");
    let open = fn_pos + match_rel + "match service {".len();
    let (block, _) = brace_block(&code, open);
    let mut svcs = BTreeSet::new();
    for (lhs, _) in parse_top_arms(block) {
        if lhs.trim() == "_" {
            continue;
        }
        for s in extract_ident_strings(lhs) {
            svcs.insert(s);
        }
    }
    svcs
}

fn collect_rs_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|x| x.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_rs_files_under(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn single_svc_delegate_for(rel: &str, fn_name: &str) -> Option<&'static [&'static str]> {
    match (rel, fn_name) {
        ("quasar_gateway_helpers.rs", "dispatch_service_gateway_mgr_rpc") => {
            Some(&["serviceGatewayMgr"])
        }
        ("quasar_gateway_helpers.rs", "dispatch_public_gateway_rpc") => Some(&["publicGatewaySvc"]),
        ("services/character_certificates.rs", "handle") => Some(&["certificateMgr"]),
        ("services/character_energy.rs", "handle") => Some(&["characterEnergyMgr"]),
        ("services/character_lifecycle.rs", "handle_char_mgr") => Some(&["charMgr"]),
        ("services/contracts_service.rs", "handle") => Some(&["contractProxy"]),
        ("services/corp_registry_ops.rs", "handle") => Some(&["corpRegistry"]),
        ("services/fitting_dogma_ops.rs", "handle_dogma_ops") => Some(&["dogmaIM"]),
        ("services/fleet_social.rs", "handle_office_manager") => Some(&["officeManager"]),
        ("services/fleet_social_access_group_bookmarks.rs", "handle") => {
            Some(&["accessGroupBookmarkMgr"])
        }
        ("services/fw_enlistment.rs", "handle") => Some(&["fwCharacterEnlistmentMgr"]),
        ("services/jump_clone_names.rs", "handle_public_qa_tools") => {
            Some(&["publicQaToolsServer"])
        }
        ("services/macho_cluster_mgmt.rs", "handle_macho_net") => Some(&["machoNet"]),
        ("services/mailing_lists_ops.rs", "handle_mail_ops") => Some(&["mailMgr"]),
        ("services/mailing_lists_ops.rs", "handle_mailing_list_ops") => {
            Some(&["mailingListsMgr"])
        }
        ("services/market_contracts.rs", "handle") => Some(&["marketProxy"]),
        ("services/market_orders_escrow.rs", "handle_orders_escrow") => Some(&["marketProxy"]),
        ("services/market_trading.rs", "handle") => Some(&["marketProxy"]),
        ("services/petitioner_ops.rs", "handle_petitioner") => Some(&["petitioner"]),
        ("services/project_discovery.rs", "handle_project_discovery") => Some(&["ProjectDiscovery"]),
        ("services/pve_dungeon_cache.rs", "handle") => Some(&["dungeonInstanceCacheMgr"]),
        ("services/serverops_cluster.rs", "handle_admin_cluster_rpc") => {
            Some(&["machoNet", "cluster"])
        }
        ("services/tourney_arena.rs", "handle") => Some(&["tourneyMgr"]),
        ("services/wallet_services_ext.rs", "handle_trade_action") => Some(&["trademgr"]),
        _ => None,
    }
}

fn enclosing_fn_name(code: &str, pos: usize) -> Option<&str> {
    let prefix = &code[..pos];
    let mut best = None;
    let mut search_from = 0;
    while let Some(rel) = prefix[search_from..].find("fn ") {
        let abs = search_from + rel;
        let ok_prev = abs == 0
            || !prefix.as_bytes()[abs - 1].is_ascii_alphanumeric()
                && prefix.as_bytes()[abs - 1] != b'_';
        if ok_prev {
            let after_fn = prefix[abs + 3..].trim_start();
            let name_len = after_fn
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            if name_len > 0 {
                let rest = after_fn[name_len..].trim_start();
                if rest.starts_with('(') || rest.starts_with('<') {
                    best = Some(&after_fn[..name_len]);
                }
            }
        }
        search_from = abs + 3;
    }
    best
}

fn scan_match_headers_and_extract(
    rel: &str,
    code: &str,
    rpcs: &mut BTreeSet<(String, String)>,
) {
    let consts = extract_file_str_consts(code);
    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if !code.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(next_i) = skip_string_or_char(bytes, i) {
            i = next_i;
            continue;
        }
        let ok_boundary =
            i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if ok_boundary && code[i..].starts_with("match") {
            let mut j = i + 5;
            if j < n && bytes[j].is_ascii_whitespace() {
                while j < n && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < n && bytes[j] == b'(' {
                    let mut p_end = j + 1;
                    let mut pd = 1usize;
                    while p_end < n && pd > 0 {
                        if let Some(nx) = skip_string_or_char(bytes, p_end) {
                            p_end = nx;
                            continue;
                        }
                        match bytes[p_end] {
                            b'(' => pd += 1,
                            b')' => pd -= 1,
                            _ => {}
                        }
                        p_end += 1;
                    }
                    if pd == 0 {
                        let inner = &code[j + 1..p_end - 1];
                        let mut k = p_end;
                        while k < n && bytes[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        if k < n && bytes[k] == b'{' {
                            if let Some((left, right)) = inner.split_once(',') {
                                let l = left.trim();
                                let r = right.trim();
                                let l_clean = l.strip_suffix(".as_str()").unwrap_or(l).trim();
                                let r_clean = r
                                    .strip_suffix(".as_str()")
                                    .map(|x| x.strip_suffix(".to_ascii_lowercase()").unwrap_or(x))
                                    .unwrap_or(r)
                                    .trim();
                                let is_svc_var =
                                    matches!(l_clean, "effective_service" | "service" | "svc");
                                let is_meth_var =
                                    matches!(r_clean, "method" | "action" | "m" | "meth");
                                let fixed_svc = if l.starts_with('"') && l.ends_with('"') {
                                    Some(l[1..l.len() - 1].to_string())
                                } else {
                                    consts.get(l_clean).cloned()
                                };
                                if (is_svc_var || fixed_svc.is_some()) && is_meth_var {
                                    let (block, _) = brace_block(code, k + 1);
                                    for (lhs, _) in parse_top_arms(block) {
                                        let pat = split_guard(lhs).0;
                                        for (s, m) in expand_tuple_or_pattern(pat, &consts) {
                                            if s == "_" {
                                                if let Some(ref fsvc) = fixed_svc {
                                                    if m != "_" {
                                                        rpcs.insert((fsvc.clone(), m));
                                                    }
                                                } else if rel == "services/fleet_social_chat.rs" {
                                                    rpcs.insert(("LSC".into(), m.clone()));
                                                    rpcs.insert(("XmppChatMgr".into(), m));
                                                }
                                            } else if m != "_" {
                                                rpcs.insert((s, m));
                                            }
                                        }
                                        if let Some((s, m)) = parse_icase_guard_pair(lhs) {
                                            rpcs.insert((s, m));
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if code[j..].starts_with("method") || code[j..].starts_with("action") {
                    let mut k = j + 6;
                    if k < n
                        && !(bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_')
                    {
                        while k < n && bytes[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        if k < n && bytes[k] == b'{' {
                            if let Some(fn_name) = enclosing_fn_name(code, i) {
                                if let Some(svcs) = single_svc_delegate_for(rel, fn_name) {
                                    let (block, _) = brace_block(code, k + 1);
                                    for (lhs, _) in parse_top_arms(block) {
                                        let pat = split_guard(lhs).0;
                                        for meth in extract_ident_strings_with_consts(pat, &consts) {
                                            for &svc in svcs {
                                                rpcs.insert((svc.to_string(), meth.clone()));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if bytes[i] == b'(' {
            // `(service, method) == ("svc", "meth")` (supports const bindings)
            if let Some(close_l) = code[i + 1..].find(')') {
                let lhs_tup = &code[i + 1..i + 1 + close_l];
                if let Some((a, b)) = lhs_tup.split_once(',') {
                    if matches!(a.trim(), "service" | "effective_service" | "svc")
                        && matches!(b.trim(), "method" | "action")
                    {
                        let after_l = code[i + 2 + close_l..].trim_start();
                        if let Some(after_eq) = after_l.strip_prefix("==") {
                            let after_eq = after_eq.trim_start();
                            if let Some(after_open) = after_eq.strip_prefix('(') {
                                if let Some(close_r) = after_open.find(')') {
                                    let rhs_tup = &after_open[..close_r];
                                    if let Some((rs, rm)) = rhs_tup.split_once(',') {
                                        if let (Some((s, _)), Some((m, _))) = (
                                            extract_leading_str_or_const(rs.trim(), &consts),
                                            extract_leading_str_or_const(rm.trim(), &consts),
                                        ) {
                                            rpcs.insert((s, m));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else if ok_boundary
            && (code[i..].starts_with("effective_service") || code[i..].starts_with("service"))
        {
            // `(effective_service|service) == ("svc"|CONST) && method == ("meth"|CONST)`
            let kw_len = if code[i..].starts_with("effective_service") {
                "effective_service".len()
            } else {
                "service".len()
            };
            let rest = code[i + kw_len..].trim_start();
            if let Some(after_eq) = rest.strip_prefix("==") {
                let after_eq = after_eq.trim_start();
                if let Some((s, after_s)) = extract_leading_str_or_const(after_eq, &consts) {
                    let tail = after_s.trim_start();
                    if let Some(after_amp) = tail.strip_prefix("&&") {
                        let after_amp = after_amp.trim_start();
                        if let Some(after_m) = after_amp.strip_prefix("method") {
                            let after_m = after_m.trim_start();
                            if let Some(after_meq) = after_m.strip_prefix("==") {
                                if let Some((m, _)) =
                                    extract_leading_str_or_const(after_meq.trim_start(), &consts)
                                {
                                    rpcs.insert((s, m));
                                }
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }
}

fn extract_leading_str_or_const<'a>(
    s: &'a str,
    consts: &HashMap<String, String>,
) -> Option<(String, &'a str)> {
    if let Some(inner) = s.strip_prefix('"') {
        let end = inner.find('"')?;
        let val = &inner[..end];
        if !val.is_empty() && val.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Some((val.to_string(), &inner[end + 1..]));
        }
        return None;
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return None;
    }
    let mut end = 1usize;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    let ident = &s[..end];
    consts.get(ident).map(|v| (v.clone(), &s[end..]))
}

#[test]
fn compiled_modules_discover_rpcs_from_registry_without_rs_scanning() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let src_root = root.join("crates/eve-server/src");
    let compiled_files: BTreeSet<&str> =
        eve_server::router::rpc_registry::compiled_rpc_module_files()
            .into_iter()
            .collect();

    let scanned_only = scan_unmigrated_match_rpcs(&src_root, &compiled_files);
    for pair in [
        ("dogmaIM", "Overload"),
        ("characterEnergyMgr", "GetMyEnergyState"),
        ("tourneyMgr", "AbortCountdown"),
        ("tourneyMgr", "TeleportToArena"),
        ("seasonManager", "get_challenge"),
        ("pvpFilamentMgr", "JoinPVPQueue"),
        ("structureCompressionMgr", "CompressItemInStructure"),
        ("ingamereport", "report_other"),
    ] {
        assert!(
            !scanned_only.contains(&(pair.0.to_string(), pair.1.to_string())),
            "compiled pair {}.{} should no longer be discovered by .rs scanning",
            pair.0,
            pair.1
        );
    }

    let (discovered, _bindable) = discover_registered_rpcs(&root);
    for (svc, method) in eve_server::router::rpc_registry::compiled_rpc_pairs() {
        assert!(
            discovered.contains(&(svc.to_string(), method.to_string())),
            "discover_registered_rpcs missing compiled entry {svc}.{method}"
        );
    }
    assert!(discovered.contains(&("dogmaIM".to_string(), "Overload".to_string())));
}

#[test]
fn compiled_rpc_match_overlaps_stay_within_shrink_only_list() {
    const SHRINK_ONLY_MATCH_OVERLAPS: &[(&str, &str)] = &[("dogmaIM", "Activate")];
    let root = workspace_root();
    let src_root = root.join("crates/eve-server/src");
    let compiled_files: BTreeSet<&str> =
        eve_server::router::rpc_registry::compiled_rpc_module_files()
            .into_iter()
            .collect();

    let scanned_only = scan_unmigrated_match_rpcs(&src_root, &compiled_files);
    let registered: BTreeSet<(String, String)> =
        eve_server::router::rpc_registry::compiled_rpc_pairs()
            .into_iter()
            .map(|(s, m)| (s.to_string(), m.to_string()))
            .collect();
    let overlaps: BTreeSet<(String, String)> =
        registered.intersection(&scanned_only).cloned().collect();
    let allowed: BTreeSet<(String, String)> = SHRINK_ONLY_MATCH_OVERLAPS
        .iter()
        .map(|(s, m)| (s.to_string(), m.to_string()))
        .collect();
    let unexpected: Vec<String> = overlaps
        .difference(&allowed)
        .map(|(s, m)| format!("{s}.{m}"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "registered RPC pairs must not newly overlap unmigrated match arms (shrink-only list): {unexpected:?}"
    );
}

fn scan_unmigrated_match_rpcs(
    src_root: &Path,
    exclude_files: &BTreeSet<&str>,
) -> BTreeSet<(String, String)> {
    let mut files = vec![
        src_root.join("router/dispatch.rs"),
        src_root.join("quasar_gateway_helpers.rs"),
    ];
    collect_rs_files_under(&src_root.join("services"), &mut files);

    let mut rpcs = BTreeSet::new();
    for path in files {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let path_str = path.to_string_lossy();
        if name.contains("_tests") || name.contains("_test") || path_str.contains("/tests") {
            continue;
        }
        let rel = path
            .strip_prefix(src_root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if exclude_files.contains(rel.as_str()) {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("read service file");
        let cleaned = strip_cfg_test_items(&strip_comments_preserving_strings(&raw));
        scan_match_headers_and_extract(&rel, &cleaned, &mut rpcs);
    }
    rpcs
}

fn discover_registered_rpcs(root: &Path) -> (BTreeSet<(String, String)>, BTreeSet<String>) {
    let src_root = root.join("crates/eve-server/src");
    let bindable = extract_bindable_services(&src_root);
    let compiled_files: BTreeSet<&str> =
        eve_server::router::rpc_registry::compiled_rpc_module_files()
            .into_iter()
            .collect();
    let mut rpcs = scan_unmigrated_match_rpcs(&src_root, &compiled_files);
    for (svc, method) in eve_server::router::rpc_registry::compiled_rpc_pairs() {
        rpcs.insert((svc.to_string(), method.to_string()));
    }

    // Bindable factory methods (`MachoBindObject`, `MachoResolveObject`) and bound dispatch helpers.
    for svc in &bindable {
        rpcs.insert((svc.clone(), "MachoBindObject".to_string()));
        rpcs.insert((svc.clone(), "MachoResolveObject".to_string()));
    }
    for meth in [
        "GetInventoryFromId",
        "GetInventory",
        "GetSelfInvItem",
        "GetAvailableTurretSlots",
    ] {
        rpcs.insert(("invbroker".to_string(), meth.to_string()));
    }
    for meth in ["IsPrimed", "GetItem"] {
        // rpc-lint: allow server-registered internal bound inventory dispatch endpoint (services/inventory_bound.rs)
        rpcs.insert(("i2".to_string(), meth.to_string()));
    }
    for svc in ["timeControl", "harness"] {
        for meth in ["FastForwardSessionTimer", "ClearSessionTimer"] {
            rpcs.insert((svc.to_string(), meth.to_string()));
        }
    }
    rpcs.insert((
        "custom_agency_provider".to_string(),
        "get_content_data".to_string(),
    ));
    rpcs.insert(("authentication".to_string(), "Ping".to_string()));
    rpcs.insert((
        "machoNet".to_string(),
        "ClientHasReleasedTheseObjects".to_string(),
    ));

    (rpcs, bindable)
}

#[derive(Debug, Clone)]
struct AllowRule {
    caller_scoped_mutation: bool,
}

const CALLER_SCOPED_MUTATION_PREFIX: &str = "caller_scoped_mutation:";

type AllowlistMap = BTreeMap<(String, String), HashMap<String, AllowRule>>;

fn load_allowlist_and_verify_parity(
    root: &Path,
    discovered: &BTreeSet<(String, String)>,
) -> AllowlistMap {
    let path = root.join("crates/eve-testkit/ratchets/rpc_negative_matrix_allowlist.json");
    let raw = std::fs::read_to_string(&path).expect("read rpc_negative_matrix_allowlist.json");
    let doc: Value = serde_json::from_str(&raw).expect("parse allowlist JSON");
    let rpcs_obj = doc
        .get("rpcs")
        .and_then(Value::as_object)
        .expect("allowlist.rpcs JSON object");

    let mut allow_map = BTreeMap::new();
    let mut allow_keys = BTreeSet::new();
    for (full_key, val) in rpcs_obj {
        let (svc, meth) = full_key
            .split_once('.')
            .unwrap_or_else(|| panic!("invalid allowlist key '{full_key}'"));
        let entry_obj = val
            .as_object()
            .unwrap_or_else(|| panic!("allowlist entry '{full_key}' must be an object"));
        let mut per_sit = HashMap::new();
        for (sit, reason_val) in entry_obj {
            assert!(
                matches!(sit.as_str(), "a" | "b" | "c" | "d" | "e" | "f"),
                "unknown situation '{sit}' in allowlist entry '{full_key}'"
            );
            let reason = reason_val
                .as_str()
                .unwrap_or_else(|| panic!("reason for '{full_key}.{sit}' must be string"));
            assert!(
                !reason.trim().is_empty() && !reason.contains('\n'),
                "reason for '{full_key}.{sit}' must be a non-empty single line"
            );
            let caller_scoped_mutation = reason.starts_with(CALLER_SCOPED_MUTATION_PREFIX);
            if caller_scoped_mutation {
                assert!(
                    sit == "b",
                    "caller_scoped_mutation prefix is only allowed on Situation 'b' (found on '{full_key}.{sit}')"
                );
                assert!(
                    !reason[CALLER_SCOPED_MUTATION_PREFIX.len()..].trim().is_empty(),
                    "caller_scoped_mutation rationale for '{full_key}.{sit}' must not be empty"
                );
            }
            per_sit.insert(
                sit.clone(),
                AllowRule {
                    caller_scoped_mutation,
                },
            );
        }
        allow_keys.insert((svc.to_string(), meth.to_string()));
        allow_map.insert((svc.to_string(), meth.to_string()), per_sit);
    }

    let missing: Vec<String> = discovered
        .difference(&allow_keys)
        .map(|(s, m)| format!("{s}.{m}"))
        .collect();
    assert!(
        missing.is_empty(),
        "Unclassified registered RPCs missing from rpc_negative_matrix_allowlist.json ({}): {:?}",
        missing.len(),
        missing
    );
    let stale: Vec<String> = allow_keys
        .difference(discovered)
        .map(|(s, m)| format!("{s}.{m}"))
        .collect();
    assert!(
        stale.is_empty(),
        "Stale keys in rpc_negative_matrix_allowlist.json not present in server dispatch ({}): {:?}",
        stale.len(),
        stale
    );

    allow_map
}

fn violations_path(root: &Path) -> PathBuf {
    root.join("crates/eve-testkit/ratchets/rpc_negative_matrix_violations.json")
}

fn load_baseline_violations(root: &Path) -> BTreeSet<MatrixViolation> {
    let raw = std::fs::read_to_string(violations_path(root))
        .expect("read rpc_negative_matrix_violations.json");
    let parsed: ViolationsRatchetFile =
        serde_json::from_str(&raw).expect("parse rpc_negative_matrix_violations.json");
    assert_eq!(parsed.version, 1, "unsupported violations ratchet version");
    assert!(
        !parsed.description.trim().is_empty(),
        "violations ratchet description must not be empty"
    );
    let mut set = BTreeSet::new();
    for g in parsed.violation_groups {
        assert!(
            !g.methods.is_empty(),
            "empty violation group ({}, {}, {})",
            g.situation,
            g.service,
            g.kind
        );
        for method in g.methods {
            let inserted = set.insert(MatrixViolation {
                situation: g.situation.clone(),
                service: g.service.clone(),
                method,
                kind: g.kind.clone(),
            });
            assert!(inserted, "duplicate violation entry in baseline ratchet");
        }
    }
    assert_eq!(
        set.len(),
        parsed.total_violations,
        "total_violations mismatch in rpc_negative_matrix_violations.json"
    );
    set
}

fn write_baseline_violations(root: &Path, observed: &BTreeSet<MatrixViolation>) {
    let mut grouped: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
    for v in observed {
        grouped
            .entry((v.situation.clone(), v.service.clone(), v.kind.clone()))
            .or_default()
            .push(v.method.clone());
    }
    let groups: Vec<ViolationGroup> = grouped
        .into_iter()
        .map(|((situation, service, kind), methods)| ViolationGroup {
            situation,
            service,
            kind,
            methods,
        })
        .collect();
    let header_desc = "Shrinking baseline ratchet of negative-matrix violations observed on main (first landing is report-only; this list may only shrink).";
    let mut out = format!(
        "{{\n  \"version\": 1,\n  \"description\": {},\n  \"total_violations\": {},\n  \"violation_groups\": [\n",
        serde_json::to_string(header_desc).expect("serialize description"),
        observed.len()
    );
    for (idx, g) in groups.iter().enumerate() {
        let line = serde_json::to_string(g).expect("serialize violation group");
        let comma = if idx + 1 < groups.len() { "," } else { "" };
        out.push_str("    ");
        out.push_str(&line);
        out.push_str(comma);
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    std::fs::write(violations_path(root), out).expect("write violations file");
}

// ============================================================================
// Seeded Multi-Actor World Fixture & Runtime State Snapshot/Restore
// ============================================================================

struct MatrixFixture {
    ctx: Arc<ServerContext>,
    _bug_dir: TempDirGuard,
    other_sys_id: SolarSystemId,
    other_station_id: i64,
    non_hq_station_id: i64,
    other_gate_id: i64,
    jita_gate_id: i64,
    a_ship_id: i64,
    b_ship_id: i64,
    b_space_ship_id: i64,
    c_ship_id: i64,
    d_ship_id: i64,
    d_space_ship_id: i64,
    e_ship_id: i64,
    e_space_ship_id: i64,
    f_ship_id: i64,
    f_space_ship_id: i64,
    a_ore_id: i64,
    c_ore_id: i64,
    d_ore_id: i64,
    e_ore_id: i64,
    f_ore_id: i64,
    c_module_id: i64,
    d_module_id: i64,
    e_module_id: i64,
    f_module_id: i64,
    b_item_id: i64,
    b_cont_id: i64,
    d_item_id: i64,
    d_cont_id: i64,
    e_item_id: i64,
    e_cont_id: i64,
    f_item_id: i64,
    f_cont_id: i64,
    b_order_id: i64,
    d_order_id: i64,
    e_order_id: i64,
    f_order_id: i64,
    b_contract_id: i64,
    d_contract_id: i64,
    e_contract_id: i64,
    f_contract_id: i64,
    b_job_id: i64,
    corp_a_job_id: i64,
    d_job_id: i64,
    e_job_id: i64,
    f_job_id: i64,
    b_blueprint_id: i64,
    corp_a_blueprint_id: i64,
    d_blueprint_id: i64,
    e_blueprint_id: i64,
    f_blueprint_id: i64,
    b_fitting_id: i64,
    d_fitting_id: i64,
    e_fitting_id: i64,
    f_fitting_id: i64,
    b_mail_id: i64,
    d_mail_id: i64,
    e_mail_id: i64,
    f_mail_id: i64,
    b_bookmark_id: i64,
    d_bookmark_id: i64,
    e_bookmark_id: i64,
    f_bookmark_id: i64,
    b_fleet_id: i64,
    d_fleet_id: i64,
    e_fleet_id: i64,
    f_fleet_id: i64,
    b_war_id: i64,
    d_war_id: i64,
    e_war_id: i64,
    f_war_id: i64,
    b_killmail_id: i64,
    d_killmail_id: i64,
    e_killmail_id: i64,
    f_killmail_id: i64,
    b_pos_id: i64,
    d_pos_id: i64,
    e_pos_id: i64,
    f_pos_id: i64,
    b_planet_id: i64,
    d_planet_id: i64,
    e_planet_id: i64,
    f_planet_id: i64,
    b_corp_project_id: i64,
    corp_a_project_id: i64,
    d_corp_project_id: i64,
    e_corp_project_id: i64,
    f_corp_project_id: i64,
    b_petition_id: i64,
    c_petition_id: i64,
    d_petition_id: i64,
    e_petition_id: i64,
    f_petition_id: i64,
    sess_by_sit: [SessionState; 6],
    actor_bounds: [HashMap<String, u64>; 6],
    b_bounds: Vec<(String, u64)>,
}

fn target_locks_sigs(
    locks: &BTreeMap<BallId, Vec<TargetLock>>,
) -> BTreeMap<BallId, Vec<(BallId, u64, u64, bool, Option<u64>)>> {
    locks
        .iter()
        .map(|(&locker, list)| {
            let sigs = list
                .iter()
                .map(|l| {
                    (
                        l.target_id,
                        l.lock_time_ms.to_bits(),
                        l.remaining_lock_ms.to_bits(),
                        l.locked,
                        l.max_range_m.map(f64::to_bits),
                    )
                })
                .collect();
            (locker, sigs)
        })
        .collect()
}

fn target_locks_b_sigs(
    locks: &BTreeMap<BallId, Vec<TargetLock>>,
    b_balls: &[BallId],
) -> BTreeMap<BallId, Vec<(BallId, u64, u64, bool, Option<u64>)>> {
    locks
        .iter()
        .filter(|(locker, list)| {
            b_balls.contains(locker) || list.iter().any(|l| b_balls.contains(&l.target_id))
        })
        .map(|(&locker, list)| {
            let sigs = list
                .iter()
                .filter(|l| b_balls.contains(&locker) || b_balls.contains(&l.target_id))
                .map(|l| {
                    (
                        l.target_id,
                        l.lock_time_ms.to_bits(),
                        l.remaining_lock_ms.to_bits(),
                        l.locked,
                        l.max_range_m.map(f64::to_bits),
                    )
                })
                .collect();
            (locker, sigs)
        })
        .collect()
}

#[derive(Clone)]
struct BallparkCheckSnapshot {
    sys_id: SolarSystemId,
    balls: BTreeMap<BallId, Ball>,
    combat_keys: Vec<(BallId, [u64; 4], [bool; 3], i32, i32, u32)>,
    combat_states: BTreeMap<BallId, CombatState>,
    warp_bubbles: WarpBubbleMgr,
    target_locks: BTreeMap<BallId, Vec<TargetLock>>,
    current_tick: u64,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct EsiRuntimeSig {
    deleted_mails: BTreeSet<(i64, i64)>,
    mail_meta: BTreeMap<(i64, i64), (bool, Vec<i64>)>,
    mail_labels: BTreeMap<i64, Vec<(i64, String, String)>>,
    mail_lists: BTreeMap<i64, Vec<(i64, String)>>,
    char_contacts: BTreeMap<i64, Vec<(i64, String, u64, bool, bool, Vec<i64>)>>,
    char_contact_labels: BTreeMap<i64, Vec<(i64, String)>>,
    corp_contacts: BTreeMap<i64, Vec<(i64, String, u64, bool, bool, Vec<i64>)>>,
    corp_contact_labels: BTreeMap<i64, Vec<(i64, String)>>,
    alliance_contacts: BTreeMap<i64, Vec<(i64, String, u64, bool, bool, Vec<i64>)>>,
    alliance_contact_labels: BTreeMap<i64, Vec<(i64, String)>>,
    calendar_events: BTreeMap<i64, (i64, u64, String, String, i64, i64, i64, String, String, Vec<i64>)>,
    calendar_responses: BTreeMap<(i64, i64), String>,
    contact_notifications: BTreeMap<i64, Vec<(i64, i64, u64, u64, String)>>,
    char_corp_history: BTreeMap<i64, Vec<(i64, i64, u64, bool)>>,
    char_titles: BTreeMap<i64, Vec<(i64, String)>>,
    fleet_motd: BTreeMap<i64, String>,
    fleet_wing_names: BTreeMap<(i64, i64), String>,
    fleet_squads: BTreeMap<(i64, i64), (i64, String)>,
    fleet_member_meta: BTreeMap<(i64, i64), (String, i64, i64)>,
    war_allies: BTreeMap<i64, Vec<Value>>,
    custom_item_names: BTreeMap<i64, String>,
}

#[derive(Clone)]
struct EsiRuntimeBaseline {
    sig: EsiRuntimeSig,
    deleted_mails: HashSet<(i64, i64)>,
    mail_meta: HashMap<(i64, i64), EsiMailMeta>,
    mail_labels: HashMap<i64, Vec<EsiMailLabel>>,
    mail_lists: HashMap<i64, Vec<(i64, String)>>,
    char_contacts: HashMap<i64, Vec<EsiContactEntry>>,
    char_contact_labels: HashMap<i64, Vec<(i64, String)>>,
    corp_contacts: HashMap<i64, Vec<EsiContactEntry>>,
    corp_contact_labels: HashMap<i64, Vec<(i64, String)>>,
    alliance_contacts: HashMap<i64, Vec<EsiContactEntry>>,
    alliance_contact_labels: HashMap<i64, Vec<(i64, String)>>,
    calendar_events: HashMap<i64, EsiCalendarEvent>,
    calendar_responses: HashMap<(i64, i64), String>,
    contact_notifications: HashMap<i64, Vec<EsiContactNotification>>,
    char_corp_history: HashMap<i64, Vec<EsiCorpHistoryEntry>>,
    char_titles: HashMap<i64, Vec<(i64, String)>>,
    fleet_motd: HashMap<i64, String>,
    fleet_wing_names: HashMap<(i64, i64), String>,
    fleet_squads: HashMap<(i64, i64), (i64, String)>,
    fleet_member_meta: HashMap<(i64, i64), (String, i64, i64)>,
    war_allies: HashMap<i64, Vec<Value>>,
    custom_item_names: HashMap<i64, String>,
}

fn contact_sig(c: &EsiContactEntry) -> (i64, String, u64, bool, bool, Vec<i64>) {
    (
        c.contact_id,
        c.contact_type.clone(),
        c.standing.to_bits(),
        c.is_watched,
        c.is_blocked,
        c.label_ids.clone(),
    )
}

fn capture_esi_sig(esi: &EsiRuntimeState) -> EsiRuntimeSig {
    // Acquire and drop one RANK_LEAF lock at a time.
    let deleted_mails = { esi.deleted_mails.read().expect("esi.deleted_mails").iter().copied().collect() };
    let mail_meta = {
        esi.mail_meta
            .read()
            .expect("esi.mail_meta")
            .iter()
            .map(|(&k, v)| (k, (v.is_read, v.labels.clone())))
            .collect()
    };
    let mail_labels = {
        esi.mail_labels
            .read()
            .expect("esi.mail_labels")
            .iter()
            .map(|(&k, vs)| {
                (
                    k,
                    vs.iter()
                        .map(|l| (l.label_id, l.name.clone(), l.color.clone()))
                        .collect(),
                )
            })
            .collect()
    };
    let mail_lists = { esi.mail_lists.read().expect("esi.mail_lists").iter().map(|(&k, v)| (k, v.clone())).collect() };
    let char_contacts = {
        esi.char_contacts
            .read()
            .expect("esi.char_contacts")
            .iter()
            .map(|(&k, vs)| (k, vs.iter().map(contact_sig).collect()))
            .collect()
    };
    let char_contact_labels = {
        esi.char_contact_labels
            .read()
            .expect("esi.char_contact_labels")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let corp_contacts = {
        esi.corp_contacts
            .read()
            .expect("esi.corp_contacts")
            .iter()
            .map(|(&k, vs)| (k, vs.iter().map(contact_sig).collect()))
            .collect()
    };
    let corp_contact_labels = {
        esi.corp_contact_labels
            .read()
            .expect("esi.corp_contact_labels")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let alliance_contacts = {
        esi.alliance_contacts
            .read()
            .expect("esi.alliance_contacts")
            .iter()
            .map(|(&k, vs)| (k, vs.iter().map(contact_sig).collect()))
            .collect()
    };
    let alliance_contact_labels = {
        esi.alliance_contact_labels
            .read()
            .expect("esi.alliance_contact_labels")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let calendar_events = {
        esi.calendar_events
            .read()
            .expect("esi.calendar_events")
            .iter()
            .map(|(&k, e)| {
                (
                    k,
                    (
                        e.event_id,
                        e.event_date_unix,
                        e.title.clone(),
                        e.text.clone(),
                        e.duration_minutes,
                        e.importance,
                        e.owner_id,
                        e.owner_name.clone(),
                        e.owner_type.clone(),
                        e.attendees.clone(),
                    ),
                )
            })
            .collect()
    };
    let calendar_responses = {
        esi.calendar_responses
            .read()
            .expect("esi.calendar_responses")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let contact_notifications = {
        esi.contact_notifications
            .read()
            .expect("esi.contact_notifications")
            .iter()
            .map(|(&k, vs)| {
                (
                    k,
                    vs.iter()
                        .map(|n| {
                            (
                                n.notification_id,
                                n.sender_character_id,
                                n.send_date_unix,
                                n.standing_level.to_bits(),
                                n.message.clone(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    };
    let char_corp_history = {
        esi.char_corp_history
            .read()
            .expect("esi.char_corp_history")
            .iter()
            .map(|(&k, vs)| {
                (
                    k,
                    vs.iter()
                        .map(|h| (h.record_id, h.corporation_id, h.start_date_unix, h.is_deleted))
                        .collect(),
                )
            })
            .collect()
    };
    let char_titles = {
        esi.char_titles
            .read()
            .expect("esi.char_titles")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let fleet_motd = { esi.fleet_motd.read().expect("esi.fleet_motd").iter().map(|(&k, v)| (k, v.clone())).collect() };
    let fleet_wing_names = {
        esi.fleet_wing_names
            .read()
            .expect("esi.fleet_wing_names")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let fleet_squads = {
        esi.fleet_squads
            .read()
            .expect("esi.fleet_squads")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let fleet_member_meta = {
        esi.fleet_member_meta
            .read()
            .expect("esi.fleet_member_meta")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let war_allies = {
        esi.war_allies
            .read()
            .expect("esi.war_allies")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    let custom_item_names = {
        esi.custom_item_names
            .read()
            .expect("esi.custom_item_names")
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    EsiRuntimeSig {
        deleted_mails,
        mail_meta,
        mail_labels,
        mail_lists,
        char_contacts,
        char_contact_labels,
        corp_contacts,
        corp_contact_labels,
        alliance_contacts,
        alliance_contact_labels,
        calendar_events,
        calendar_responses,
        contact_notifications,
        char_corp_history,
        char_titles,
        fleet_motd,
        fleet_wing_names,
        fleet_squads,
        fleet_member_meta,
        war_allies,
        custom_item_names,
    }
}

fn capture_esi_baseline(esi: &EsiRuntimeState) -> EsiRuntimeBaseline {
    let sig = capture_esi_sig(esi);
    let deleted_mails = { esi.deleted_mails.read().expect("esi.deleted_mails").clone() };
    let mail_meta = { esi.mail_meta.read().expect("esi.mail_meta").clone() };
    let mail_labels = { esi.mail_labels.read().expect("esi.mail_labels").clone() };
    let mail_lists = { esi.mail_lists.read().expect("esi.mail_lists").clone() };
    let char_contacts = { esi.char_contacts.read().expect("esi.char_contacts").clone() };
    let char_contact_labels = { esi.char_contact_labels.read().expect("esi.char_contact_labels").clone() };
    let corp_contacts = { esi.corp_contacts.read().expect("esi.corp_contacts").clone() };
    let corp_contact_labels = { esi.corp_contact_labels.read().expect("esi.corp_contact_labels").clone() };
    let alliance_contacts = { esi.alliance_contacts.read().expect("esi.alliance_contacts").clone() };
    let alliance_contact_labels = { esi.alliance_contact_labels.read().expect("esi.alliance_contact_labels").clone() };
    let calendar_events = { esi.calendar_events.read().expect("esi.calendar_events").clone() };
    let calendar_responses = { esi.calendar_responses.read().expect("esi.calendar_responses").clone() };
    let contact_notifications = { esi.contact_notifications.read().expect("esi.contact_notifications").clone() };
    let char_corp_history = { esi.char_corp_history.read().expect("esi.char_corp_history").clone() };
    let char_titles = { esi.char_titles.read().expect("esi.char_titles").clone() };
    let fleet_motd = { esi.fleet_motd.read().expect("esi.fleet_motd").clone() };
    let fleet_wing_names = { esi.fleet_wing_names.read().expect("esi.fleet_wing_names").clone() };
    let fleet_squads = { esi.fleet_squads.read().expect("esi.fleet_squads").clone() };
    let fleet_member_meta = { esi.fleet_member_meta.read().expect("esi.fleet_member_meta").clone() };
    let war_allies = { esi.war_allies.read().expect("esi.war_allies").clone() };
    let custom_item_names = { esi.custom_item_names.read().expect("esi.custom_item_names").clone() };
    EsiRuntimeBaseline {
        sig,
        deleted_mails,
        mail_meta,
        mail_labels,
        mail_lists,
        char_contacts,
        char_contact_labels,
        corp_contacts,
        corp_contact_labels,
        alliance_contacts,
        alliance_contact_labels,
        calendar_events,
        calendar_responses,
        contact_notifications,
        char_corp_history,
        char_titles,
        fleet_motd,
        fleet_wing_names,
        fleet_squads,
        fleet_member_meta,
        war_allies,
        custom_item_names,
    }
}

fn restore_esi_runtime(esi: &EsiRuntimeState, base: &EsiRuntimeBaseline) {
    *esi.deleted_mails.write().expect("esi.deleted_mails") = base.deleted_mails.clone();
    *esi.mail_meta.write().expect("esi.mail_meta") = base.mail_meta.clone();
    *esi.mail_labels.write().expect("esi.mail_labels") = base.mail_labels.clone();
    *esi.mail_lists.write().expect("esi.mail_lists") = base.mail_lists.clone();
    *esi.char_contacts.write().expect("esi.char_contacts") = base.char_contacts.clone();
    *esi.char_contact_labels.write().expect("esi.char_contact_labels") = base.char_contact_labels.clone();
    *esi.corp_contacts.write().expect("esi.corp_contacts") = base.corp_contacts.clone();
    *esi.corp_contact_labels.write().expect("esi.corp_contact_labels") = base.corp_contact_labels.clone();
    *esi.alliance_contacts.write().expect("esi.alliance_contacts") = base.alliance_contacts.clone();
    *esi.alliance_contact_labels.write().expect("esi.alliance_contact_labels") = base.alliance_contact_labels.clone();
    *esi.calendar_events.write().expect("esi.calendar_events") = base.calendar_events.clone();
    *esi.calendar_responses.write().expect("esi.calendar_responses") = base.calendar_responses.clone();
    *esi.contact_notifications.write().expect("esi.contact_notifications") = base.contact_notifications.clone();
    *esi.char_corp_history.write().expect("esi.char_corp_history") = base.char_corp_history.clone();
    *esi.char_titles.write().expect("esi.char_titles") = base.char_titles.clone();
    *esi.fleet_motd.write().expect("esi.fleet_motd") = base.fleet_motd.clone();
    *esi.fleet_wing_names.write().expect("esi.fleet_wing_names") = base.fleet_wing_names.clone();
    *esi.fleet_squads.write().expect("esi.fleet_squads") = base.fleet_squads.clone();
    *esi.fleet_member_meta.write().expect("esi.fleet_member_meta") = base.fleet_member_meta.clone();
    *esi.war_allies.write().expect("esi.war_allies") = base.war_allies.clone();
    *esi.custom_item_names.write().expect("esi.custom_item_names") = base.custom_item_names.clone();
}

/// Excludes strictly passive caller-presence broadcasts sent to Jita bystanders
/// (`CHAR_B_ID`) when `CHAR_A_ID` logs off or undocks from shared Jita 4-4; Target B's
/// own local-chat membership is still enforced directly via `WorldStateSnapshot.chat_channels`.
fn is_passive_caller_presence_notification(event: &str, args: &[PyRep]) -> bool {
    match event {
        "OnChannelLeft" => {
            matches!(args, [PyRep::String(ch)] if ch.starts_with("solarsystemid2:"))
                || matches!(args, [PyRep::Bytes(ch)] if ch.starts_with(b"solarsystemid2:"))
        }
        "OnCharNoLongerInStation" => matches!(
            args,
            [PyRep::Tuple(xs)] if matches!(xs.first(), Some(PyRep::Integer(cid)) if *cid == CHAR_A_ID)
        ),
        _ => false,
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct TargetBRuntimeSlice {
    ship_fitting_keys_present: BTreeSet<i64>,
    ship_fit_revs: BTreeMap<i64, eve_dogma::FitRev>,
    brain_version: Option<i64>,
    weapon_cycles: BTreeMap<i64, (Option<u64>, u32, Option<i64>)>,
    projected_ewar_lens: BTreeMap<i64, usize>,
    projected_ewar_sources: BTreeMap<i64, i64>,
    active_ewar_starts: BTreeMap<(i64, i64), usize>,
    mining_cycles_present: BTreeSet<i64>,
    module_cooldowns: BTreeMap<(i64, i64, u32), (u64, u64, Option<u32>, u64)>,
    scanner_probes: BTreeSet<i64>,
    tactical_entries: (usize, usize, usize, usize, usize),
    pending_notifications_b_sigs: Vec<(String, String)>,
    combat_runtime_b: CombatRuntimeBSig,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct CombatRuntimeBSig {
    duel_challenges: BTreeMap<(i64, i64), u64>,
    corp_aggression: Option<bool>,
    disapproval_flag: Option<u64>,
    safety_level: Option<String>,
    limited_engagements: BTreeMap<(i64, i64), u64>,
    npc_timer: Option<u64>,
    timer_states: Option<(i64, i64, i64, i64, i64)>,
    timer_triggered: Option<[u64; 5]>,
    sec_txs_len: usize,
    detailed_killmail_ids: BTreeSet<i64>,
    bounty_pools: BTreeMap<i64, u64>,
    bounty_claims: BTreeMap<i64, u64>,
    self_destruct_ships: BTreeMap<i64, (u64, u64)>,
    self_destruct_gens: BTreeMap<i64, u64>,
    drones_count: usize,
    drone_miners_count: usize,
    behavior_debug_b: bool,
    authorized_storage_b: bool,
    entropic_spools: BTreeMap<(i64, i64), String>,
    active_breacher_dots: BTreeMap<i64, String>,
    in_flight_missiles: BTreeMap<i64, String>,
}

fn capture_combat_runtime_b(fix: &MatrixFixture) -> CombatRuntimeBSig {
    let b_ships = [fix.b_ship_id, fix.b_space_ship_id, CORP_B_STRUCTURE_ID, fix.b_pos_id];
    let b_owners = [CHAR_B_ID, CORP_B_ID, ALLIANCE_B_ID];
    eve_server::services::combat_crimewatch::with_combat_runtime(&fix.ctx, |rt| CombatRuntimeBSig {
        duel_challenges: rt
            .duel_challenges
            .iter()
            .filter(|(&(a, b), _)| a == CHAR_B_ID || b == CHAR_B_ID)
            .map(|(&k, &v)| (k, v))
            .collect(),
        corp_aggression: rt.corp_aggression_settings.get(&CORP_B_ID).copied(),
        disapproval_flag: rt.disapproval_flags.get(&CHAR_B_ID).copied(),
        safety_level: rt.safety_levels.get(&CHAR_B_ID).map(|s| format!("{s:?}")),
        limited_engagements: rt
            .limited_engagements
            .iter()
            .flat_map(|(&k1, inner)| {
                inner.iter().filter_map(move |(&k2, &v)| {
                    (k1 == CHAR_B_ID || k2 == CHAR_B_ID).then_some(((k1, k2), v))
                })
            })
            .collect(),
        npc_timer: rt.npc_timers.get(&CHAR_B_ID).copied(),
        timer_states: rt.crimewatch_timer_states.get(&CHAR_B_ID).copied(),
        timer_triggered: rt.crimewatch_timer_triggered_at.get(&CHAR_B_ID).copied(),
        sec_txs_len: rt.sec_status_transactions.get(&CHAR_B_ID).map_or(0, Vec::len),
        detailed_killmail_ids: rt
            .detailed_killmails
            .iter()
            .filter(|(&kid, km)| {
                kid == fix.b_killmail_id
                    || km.victim_character_id == CHAR_B_ID
                    || km.victim_corporation_id == CORP_B_ID
            })
            .map(|(&kid, _)| kid)
            .collect(),
        bounty_pools: b_owners
            .iter()
            .filter_map(|&id| rt.player_bounty_pool.get(&id).map(|v| (id, v.to_bits())))
            .collect(),
        bounty_claims: b_owners
            .iter()
            .filter_map(|&id| rt.bounty_hunter_claimed.get(&id).map(|v| (id, v.to_bits())))
            .collect(),
        self_destruct_ships: b_ships
            .iter()
            .filter_map(|&sid| {
                rt.self_destruct_countdowns
                    .get(&sid)
                    .map(|sd| (sid, (sd.started_unix, sd.detonate_at_unix)))
            })
            .collect(),
        self_destruct_gens: b_ships
            .iter()
            .filter_map(|&sid| rt.self_destruct_generations.get(&sid).map(|&g| (sid, g)))
            .collect(),
        drones_count: rt
            .active_drones_in_space
            .values()
            .filter(|d| d.owner_char_id == CHAR_B_ID || b_ships.contains(&d.parent_ship_id))
            .count(),
        drone_miners_count: rt
            .active_drone_miners
            .values()
            .filter(|m| m.owner_char_id == CHAR_B_ID || b_ships.contains(&m.ship_id))
            .count(),
        behavior_debug_b: b_ships.iter().any(|s| rt.behavior_debugging_active.contains(s))
            || rt.behavior_debugging_active.contains(&CHAR_B_ID),
        authorized_storage_b: rt
            .authorized_shipping_storage
            .iter()
            .any(|&(cid, oid)| cid == CHAR_B_ID || oid == CORP_B_ID),
        entropic_spools: rt
            .entropic_spools
            .iter()
            .filter(|(&(s, t), _)| b_ships.contains(&s) || b_ships.contains(&t))
            .map(|(&k, v)| (k, format!("{v:?}")))
            .collect(),
        active_breacher_dots: rt
            .active_breacher_dots
            .iter()
            .filter(|(&tid, d)| {
                b_ships.contains(&tid) || d.source_char_id == CHAR_B_ID || b_ships.contains(&d.source_ship_id)
            })
            .map(|(&k, v)| (k, format!("{v:?}")))
            .collect(),
        in_flight_missiles: rt
            .in_flight_missiles
            .iter()
            .filter(|(_, m)| {
                m.attacker_char_id == CHAR_B_ID
                    || b_ships.contains(&m.attacker_ship_id)
                    || b_ships.contains(&m.target_ball_id)
            })
            .map(|(&k, v)| (k, format!("{v:?}")))
            .collect(),
    })
}

fn capture_target_b_runtime(fix: &MatrixFixture) -> TargetBRuntimeSlice {
    let b_ships = [fix.b_ship_id, fix.b_space_ship_id, CORP_B_STRUCTURE_ID, fix.b_pos_id];
    let ship_fitting_keys_present = {
        let sf = fix.ctx.ship_fittings.read().expect("ship_fittings");
        b_ships.iter().copied().filter(|id| sf.contains_key(id)).collect()
    };
    let ship_fit_revs = {
        let sr = fix.ctx.ship_fit_revs.read().expect("ship_fit_revs");
        b_ships.iter().filter_map(|&id| sr.get(&id).map(|&r| (id, r))).collect()
    };
    let brain_version = {
        fix.ctx.brain_versions.read().expect("brain_versions").get(&CHAR_B_ID).copied()
    };
    let weapon_cycles = {
        let wc = fix.ctx.weapon_cycle_state.read().expect("weapon_cycle_state");
        b_ships.iter().filter_map(|&id| wc.get(&id).cloned().map(|v| (id, v))).collect()
    };
    let projected_ewar_lens = {
        let pe = fix.ctx.projected_ewar.read().expect("projected_ewar");
        b_ships.iter().filter_map(|&id| pe.get(&id).map(|v| (id, v.len()))).collect()
    };
    let projected_ewar_sources = {
        let ps = fix.ctx.projected_ewar_sources.read().expect("projected_ewar_sources");
        ps.iter()
            .filter(|&(&m, &s)| b_ships.contains(&m) || b_ships.contains(&s))
            .map(|(&k, &v)| (k, v))
            .collect()
    };
    let active_ewar_starts = {
        let ae = fix.ctx.active_ewar_starts.read().expect("active_ewar_starts");
        ae.iter()
            .filter(|(&(s, t), _)| b_ships.contains(&s) || b_ships.contains(&t))
            .map(|(&k, v)| (k, v.len()))
            .collect()
    };
    let mining_cycles_present = {
        let mc = fix.ctx.mining_cycles.read().expect("mining_cycles");
        mc.iter()
            .filter(|(_, c)| c.char_id == CHAR_B_ID || b_ships.contains(&c.ship_id))
            .map(|(&mid, _)| mid)
            .collect()
    };
    let module_cooldowns = {
        let cd = fix.ctx.module_reactivation_cooldowns.read().expect("module_cooldowns");
        cd.iter()
            .filter(|(&(sid, _, _), _)| b_ships.contains(&sid))
            .map(|(&k, v)| {
                (
                    k,
                    (
                        v.recorded_unix_f64.to_bits(),
                        v.delay_s.to_bits(),
                        v.solar_system_id,
                        v.recorded_sys_sim_s.to_bits(),
                    ),
                )
            })
            .collect()
    };
    let scanner_probes = {
        let sp = fix.ctx.active_scanner_probes.read().expect("scanner_probes");
        sp.iter()
            .filter(|(_, p)| p.owner_char_id == CHAR_B_ID || b_ships.contains(&p.ship_id))
            .map(|(&pid, _)| pid)
            .collect()
    };
    let tactical_entries = {
        let ts = fix.ctx.tactical_state.read().expect("tactical_state");
        (
            ts.remote_assistance.iter().filter(|(&(_, sid), _)| b_ships.contains(&sid)).count(),
            ts.lance_caster_lock_until.iter().filter(|(&(_, sid), _)| b_ships.contains(&sid)).count(),
            ts.lance_victim_debuff_until.iter().filter(|(&(_, sid), _)| b_ships.contains(&sid)).count(),
            ts.entosis_active_links
                .iter()
                .filter(|(&(_, sid), l)| {
                    b_ships.contains(&sid) || l.char_id == CHAR_B_ID || b_ships.contains(&l.target_id)
                })
                .count(),
            ts.entosis_target_cycles.iter().filter(|(&(_, sid, tid), _)| b_ships.contains(&sid) || b_ships.contains(&tid)).count(),
        )
    };
    let pending_notifications_b_sigs = {
        fix.ctx
            .pending_notifications
            .read()
            .expect("pending_notifications")
            .get(&CHAR_B_ID)
            .map(|notifs| {
                notifs
                    .iter()
                    .filter(|n| !is_passive_caller_presence_notification(&n.event, &n.args))
                    .map(|n| (n.event.clone(), format!("{:?}", n.args)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let combat_runtime_b = capture_combat_runtime_b(fix);
    TargetBRuntimeSlice {
        ship_fitting_keys_present,
        ship_fit_revs,
        brain_version,
        weapon_cycles,
        projected_ewar_lens,
        projected_ewar_sources,
        active_ewar_starts,
        mining_cycles_present,
        module_cooldowns,
        scanner_probes,
        tactical_entries,
        pending_notifications_b_sigs,
        combat_runtime_b,
    }
}

#[derive(Clone)]
struct RuntimeBaseline {
    db: WorldTablesSnapshot,
    bound_keys: HashMap<u64, (u64, u64, i64, String, i64)>,
    bound_entries: HashMap<u64, BoundObjectEntry>,
    warp_states: BTreeMap<u32, eve_server::system_actor::ActorWarpState>,
    location_change_invuln: HashMap<i64, u64>,
    global_config_values: HashMap<String, PyRep>,
    ephemeral_counts: [usize; 8],
    clock_unix: u64,
    ballparks: Vec<BallparkCheckSnapshot>,
    esi: EsiRuntimeBaseline,
    crimewatch_b_state: Option<PersistedCrimeWatchState>,
    crimewatch_aggressions: BTreeMap<(i64, i64), PersistedAggressionRecord>,
    crimewatch_pending_despawns: Vec<(u32, i64, u64)>,
    crimewatch_states_len: usize,
    trashed_mails: BTreeSet<(i64, i64)>,
    b_fleet_has_extras: bool,
    target_b_runtime: TargetBRuntimeSlice,
}

fn combat_sig(cs: &CombatState) -> ([u64; 4], [bool; 3], i32, i32, u32) {
    (
        [
            cs.shield_hp.to_bits(),
            cs.armor_hp.to_bits(),
            cs.structure_hp.to_bits(),
            cs.capacitor.to_bits(),
        ],
        [cs.is_cloaked, cs.is_tethered, cs.is_covert_cloak],
        cs.warp_scramble_strength,
        cs.warp_core_strength,
        cs.gate_cloak_remaining_ticks,
    )
}

fn capture_ballpark_snapshot(ctx: &ServerContext, sys_id: SolarSystemId) -> BallparkCheckSnapshot {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let bp = sys.ballpark.read().expect("read ballpark");
    let combat_keys = bp
        .combat_states
        .iter()
        .map(|(&id, cs)| {
            let sig = combat_sig(cs);
            (id, sig.0, sig.1, sig.2, sig.3, sig.4)
        })
        .collect();
    BallparkCheckSnapshot {
        sys_id,
        balls: bp.balls.clone(),
        combat_keys,
        combat_states: bp.combat_states.clone(),
        warp_bubbles: bp.warp_bubble_mgr.clone(),
        target_locks: bp.target_locks.clone(),
        current_tick: bp.current_tick,
    }
}

fn current_ephemeral_counts(ctx: &ServerContext) -> [usize; 8] {
    let c0 = ctx.mining_cycles.read().expect("mining_cycles").len();
    let c1 = ctx.active_scanner_probes.read().expect("probes").len();
    let c2 = ctx.active_moon_probes.read().expect("moon_probes").len();
    let c3 = ctx
        .phased_asteroid_fields
        .read()
        .expect("phased_fields")
        .len();
    let c4 = ctx.weapon_cycle_state.read().expect("weapon_cycles").len();
    let c5 = ctx.projected_ewar.read().expect("projected_ewar").len();
    let c6 = ctx
        .active_ewar_starts
        .read()
        .expect("active_ewar_starts")
        .len();
    let c7 = ctx
        .session_change_timers
        .lock()
        .expect("session_change_timers")
        .len();
    [c0, c1, c2, c3, c4, c5, c6, c7]
}

fn capture_warp_states(ctx: &ServerContext) -> BTreeMap<u32, eve_server::system_actor::ActorWarpState> {
    ctx.systems
        .active_system_handles()
        .into_iter()
        .filter_map(|(sid, h)| {
            let st = h.snapshot_warp_state_for_test();
            (!st.provenance.is_empty()
                || !st.active_warps.is_empty()
                || !st.return_warps.is_empty()
                || !st.pending_warp_finished.is_empty())
            .then_some((sid, st))
        })
        .collect()
}

fn capture_runtime_baseline(fix: &MatrixFixture) -> RuntimeBaseline {
    let db = fix.ctx.db.snapshot_tables();
    let bound_entries = fix
        .ctx
        .bound_objects
        .read()
        .expect("bound_objects")
        .clone();
    let bound_keys = bound_entries
        .iter()
        .map(|(&id, e)| {
            (
                id,
                (
                    e.session_id,
                    e.user_id,
                    e.character_id,
                    e.service.clone(),
                    e.object_id,
                ),
            )
        })
        .collect();
    let warp_states = capture_warp_states(&fix.ctx);
    let location_change_invuln = fix.ctx.systems.snapshot_location_change_invuln();
    let global_config_values = fix
        .ctx
        .global_config_values
        .read()
        .expect("global_config_values")
        .clone();
    let ephemeral_counts = current_ephemeral_counts(&fix.ctx);
    let clock_unix = fix.ctx.clock().now_unix_secs();
    let ballparks = vec![
        capture_ballpark_snapshot(&fix.ctx, SYS_JITA),
        capture_ballpark_snapshot(&fix.ctx, fix.other_sys_id),
    ];
    let esi = capture_esi_baseline(&fix.ctx.esi_state);
    let crimewatch_b_state = fix.ctx.crimewatch.state_of(CHAR_B_ID);
    let (crimewatch_states_len, crimewatch_aggressions, crimewatch_pending_despawns) = {
        let cw = fix.ctx.crimewatch.read().expect("crimewatch");
        (
            cw.states.len(),
            cw.aggression_mgr
                .records
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect(),
            cw.pending_concord_despawns.clone(),
        )
    };
    let trashed_mails: BTreeSet<(i64, i64)> =
        eve_server::services::fleet_social::get_trashed_mails(&fix.ctx)
            .into_iter()
            .collect();
    let b_fleet_has_extras = fix.ctx.has_fleet_extras_for(fix.b_fleet_id);
    let target_b_runtime = capture_target_b_runtime(fix);
    RuntimeBaseline {
        db,
        bound_keys,
        bound_entries,
        warp_states,
        location_change_invuln,
        global_config_values,
        ephemeral_counts,
        clock_unix,
        ballparks,
        esi,
        crimewatch_b_state,
        crimewatch_aggressions,
        crimewatch_pending_despawns,
        crimewatch_states_len,
        trashed_mails,
        b_fleet_has_extras,
        target_b_runtime,
    }
}

fn check_runtime_changed(
    fix: &MatrixFixture,
    base: &RuntimeBaseline,
) -> (bool, bool, bool) {
    let bound_dirty = {
        let cur = fix.ctx.bound_objects.read().expect("bound_objects");
        cur.len() != base.bound_keys.len()
            || cur.iter().any(|(&id, e)| {
                base.bound_keys.get(&id).is_none_or(|b| {
                    b.0 != e.session_id
                        || b.1 != e.user_id
                        || b.2 != e.character_id
                        || b.3 != e.service
                        || b.4 != e.object_id
                })
            })
    };
    let warp_states_dirty = capture_warp_states(&fix.ctx) != base.warp_states;
    let invuln_dirty = fix.ctx.systems.snapshot_location_change_invuln() != base.location_change_invuln;
    let warps_dirty = warp_states_dirty || invuln_dirty;
    let config_dirty = *fix
        .ctx
        .global_config_values
        .read()
        .expect("global_config_values")
        != base.global_config_values;
    let counts = current_ephemeral_counts(&fix.ctx);
    let eph_dirty = counts != base.ephemeral_counts;
    let clock_dirty = fix.ctx.clock().now_unix_secs() != base.clock_unix;
    let esi_dirty = capture_esi_sig(&fix.ctx.esi_state) != base.esi.sig;
    let cw_dirty = fix.ctx.crimewatch.state_of(CHAR_B_ID) != base.crimewatch_b_state || {
        let cw = fix.ctx.crimewatch.read().expect("crimewatch");
        let aggrs: BTreeMap<(i64, i64), PersistedAggressionRecord> = cw
            .aggression_mgr
            .records
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        cw.states.len() != base.crimewatch_states_len
            || aggrs != base.crimewatch_aggressions
            || cw.pending_concord_despawns != base.crimewatch_pending_despawns
    };
    let social_trashed_dirty = {
        let cur: BTreeSet<(i64, i64)> =
            eve_server::services::fleet_social::get_trashed_mails(&fix.ctx)
                .into_iter()
                .collect();
        cur != base.trashed_mails
    };
    let fleet_extras_dirty = fix.ctx.has_fleet_extras_for(fix.b_fleet_id) != base.b_fleet_has_extras;
    let target_b_rt_dirty = capture_target_b_runtime(fix) != base.target_b_runtime;

    let mut space_dirty = false;
    for bp_base in &base.ballparks {
        let sys = fix.ctx.systems.get_or_spawn(bp_base.sys_id);
        let bp = sys.ballpark.read().expect("ballpark");
        let cur_combat_changed = bp.combat_states.len() != bp_base.combat_keys.len()
            || bp_base
                .combat_keys
                .iter()
                .any(|(id, floats, bools, scram, core, gate)| {
                    bp.combat_states.get(id).is_none_or(|cs| {
                        let sig = combat_sig(cs);
                        sig.0 != *floats
                            || sig.1 != *bools
                            || sig.2 != *scram
                            || sig.3 != *core
                            || sig.4 != *gate
                    })
                });
        if bp.balls != bp_base.balls
            || cur_combat_changed
            || bp.warp_bubble_mgr != bp_base.warp_bubbles
            || target_locks_sigs(&bp.target_locks) != target_locks_sigs(&bp_base.target_locks)
            || bp.current_tick != bp_base.current_tick
        {
            space_dirty = true;
            break;
        }
    }

    (
        bound_dirty
            || warps_dirty
            || config_dirty
            || eph_dirty
            || clock_dirty
            || esi_dirty
            || cw_dirty
            || social_trashed_dirty
            || fleet_extras_dirty
            || target_b_rt_dirty
            || space_dirty,
        bound_dirty,
        space_dirty,
    )
}

async fn restore_world_state(
    fix: &MatrixFixture,
    base: &RuntimeBaseline,
    db_dirty: bool,
    bound_dirty: bool,
    space_dirty: bool,
) {
    if db_dirty {
        fix.ctx.db.restore(&base.db).expect("restore DB snapshot");
        eve_server::services::combat_crimewatch::sync_combat_runtime_from_db(&fix.ctx);
    }
    eve_server::services::fleet_social::hydrate_social_state_from_db(&fix.ctx);
    restore_esi_runtime(&fix.ctx.esi_state, &base.esi);
    if bound_dirty {
        *fix.ctx.bound_objects.write().expect("bound_objects") = base.bound_entries.clone();
    }
    for (sid, h) in fix.ctx.systems.active_system_handles() {
        if let Some(st) = base.warp_states.get(&sid) {
            h.restore_warp_state_for_test(st.clone());
        } else {
            h.clear_all_warp_tracking_for_test();
        }
    }
    fix.ctx
        .systems
        .restore_location_change_invuln(base.location_change_invuln.clone());
    *fix
        .ctx
        .global_config_values
        .write()
        .expect("global_config_values") = base.global_config_values.clone();
    fix.ctx.mining_cycles.write().expect("mining_cycles").clear();
    fix.ctx
        .active_scanner_probes
        .write()
        .expect("probes")
        .clear();
    fix.ctx
        .active_moon_probes
        .write()
        .expect("moon_probes")
        .clear();
    fix.ctx
        .phased_asteroid_fields
        .write()
        .expect("phased_fields")
        .clear();
    fix.ctx
        .weapon_cycle_state
        .write()
        .expect("weapon_cycles")
        .clear();
    fix.ctx
        .projected_ewar
        .write()
        .expect("projected_ewar")
        .clear();
    fix.ctx
        .active_ewar_starts
        .write()
        .expect("active_ewar_starts")
        .clear();
    fix.ctx
        .pending_notifications
        .write()
        .expect("pending_notifications")
        .clear();
    fix.ctx
        .notification_records
        .write()
        .expect("notification_records")
        .clear();
    fix.ctx
        .session_change_timers
        .lock()
        .expect("session_change_timers")
        .clear();
    if fix.ctx.clock().now_unix_secs() != base.clock_unix {
        fix.ctx
            .set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
                base.clock_unix,
            )));
    }

    if space_dirty {
        for bp_base in &base.ballparks {
            let sys = fix.ctx.systems.get_or_spawn(bp_base.sys_id);
            {
                let mut bp = sys.ballpark.write().expect("write ballpark");
                bp.balls = bp_base.balls.clone();
                bp.combat_states = bp_base.combat_states.clone();
                bp.warp_bubble_mgr = bp_base.warp_bubbles.clone();
                bp.target_locks = bp_base.target_locks.clone();
                bp.pending_events.clear();
                bp.observer_events.clear();
                bp.scheduled_observer_events.clear();
                bp.current_tick = bp_base.current_tick;
            }
        }
        let sys_jita = fix.ctx.systems.get_or_spawn(SYS_JITA);
        let _ = sys_jita.set_warp_scramble_strength(fix.f_ship_id, 5).await;
        let sys_other = fix.ctx.systems.get_or_spawn(fix.other_sys_id);
        let _ = sys_other.set_warp_core_strength(fix.e_ship_id, 1).await;
    }
}

fn find_system_gate_and_station(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (Option<i64>, Option<DVec3>, Option<i64>, Option<DVec3>) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let bp = sys.ballpark.read().expect("read ballpark");
    let mut gate = None;
    let mut station = None;
    for (&id, meta) in &bp.ball_slim_meta {
        if meta.group_id == 10 && gate.is_none() {
            gate = bp.balls.get(&id).map(|b| (id, b.position));
        } else if meta.group_id == 15 && station.is_none() {
            station = bp.balls.get(&id).map(|b| (id, b.position));
        }
    }
    (
        gate.map(|x| x.0),
        gate.map(|x| x.1),
        station.map(|x| x.0),
        station.map(|x| x.1),
    )
}

async fn bind_service_for_session(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    object_id: i64,
) -> Option<u64> {
    let mut h = common::borrow_wire_harness(ctx, sess).with_mode(ContractValidationMode::ReportOnly);
    let out = h
        .call_exchange(
            svc,
            None,
            "MachoBindObject",
            &[PyRep::Integer(object_id), PyRep::None],
            &HashMap::new(),
        )
        .await;
    out.result
        .ok()
        .as_ref()
        .and_then(eve_testkit::client_harness::extract_bound_id_from_wire_rep)
}

async fn setup_fixture(sde: Arc<SdeRegistry>) -> MatrixFixture {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(
        eve_server::router::TestContext::with_sde_and_repository(sde, db)
            .with_rpc_rate_budget_disabled()
            .into_server(),
    );
    common::pin_clock(&ctx);
    let bug_dir = TempDirGuard::new("evomu_neg_matrix_bugs");
    ctx.bug_reporting
        .set_queue_dir_override(Some(bug_dir.path.clone()));

    let jita_sys = ctx
        .sde
        .solar_systems
        .get(&SYS_JITA)
        .expect("Jita solar system in SDE");
    let jita_constellation = jita_sys.constellation_id;
    let jita_region = jita_sys.region_id;

    let other_sys = ctx
        .sde
        .solar_systems
        .get(&PREFERRED_OTHER_SYS)
        .filter(|s| !s.stations.is_empty())
        .or_else(|| {
            ctx.sde
                .solar_systems
                .values()
                .find(|s| s.solar_system_id != SYS_JITA && !s.stations.is_empty())
        })
        .expect("second solar system with a station");
    let other_sys_id = other_sys.solar_system_id;
    let other_constellation = other_sys.constellation_id;
    let other_region = other_sys.region_id;
    let other_station_id = other_sys.stations[0].station_id;
    // Non-Pochven station that is neither Corp E's HQ (`other_station_id`) nor `CHAR_E_ID`'s school station (`school_id = 11`).
    let school_11_station_id = ctx.sde.schools.get(&11).map(|s| s.station_id);
    let non_hq_station_id = if Some(NPC_STATION_ID) != school_11_station_id
        && NPC_STATION_ID != other_station_id
    {
        NPC_STATION_ID
    } else {
        ctx.sde
            .solar_systems
            .values()
            .filter(|s| s.region_id != 10_000_070)
            .flat_map(|s| s.stations.iter().map(|st| st.station_id))
            .find(|&sid| sid != other_station_id && Some(sid) != school_11_station_id)
            .expect("non-HQ, non-school station present in SDE")
    };
    let b_planet_id = jita_sys.planet_ids.first().copied().unwrap_or(40_009_001);
    let d_planet_id = jita_sys.planet_ids.get(1).copied().unwrap_or(b_planet_id);
    let e_planet_id = other_sys.planet_ids.first().copied().unwrap_or(40_009_002);
    let f_planet_id = jita_sys.planet_ids.get(2).copied().unwrap_or(b_planet_id);
    let bp_type_id = *ctx
        .sde
        .blueprints
        .keys()
        .min()
        .expect("at least one blueprint in SDE");

    // Provision strictly non-GM player accounts (`role = ROLE_PLAYER_DEFAULT`).
    let actor_specs = [
        ("neg_a", USER_A_ID, CHAR_A_ID, "Neg Pilot A", 1_000_009, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
        ("neg_b", USER_B_ID, CHAR_B_ID, "Neg Pilot B", CORP_B_ID, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
        ("neg_c", USER_C_ID, CHAR_C_ID, "Neg Pilot C", CORP_A_ID, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
        ("neg_d", USER_D_ID, CHAR_D_ID, "Neg Pilot D", CORP_D_ID, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
        ("neg_e", USER_E_ID, CHAR_E_ID, "Neg Pilot E", CORP_E_ID, Some(other_station_id), other_sys_id, other_constellation, other_region),
        ("neg_f", USER_F_ID, CHAR_F_ID, "Neg Pilot F", CORP_F_ID, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
        ("neg_ceo_a", USER_A_CEO_ID, CHAR_A_CEO_ID, "Neg CEO A", CORP_A_ID, Some(NPC_STATION_ID), SYS_JITA, jita_constellation, jita_region),
    ];

    let mut alloc = 9_200_000i64;
    let mut next_id = || {
        alloc += 1;
        alloc
    };

    let a_ship_id = next_id();
    let b_ship_id = next_id();
    let b_space_ship_id = next_id();
    let c_ship_id = next_id();
    let d_ship_id = next_id();
    let d_space_ship_id = next_id();
    let e_ship_id = next_id();
    let e_space_ship_id = next_id();
    let f_ship_id = next_id();
    let f_space_ship_id = next_id();
    let ceo_ship_id = next_id();
    let a_ore_id = next_id();
    let c_ore_id = next_id();
    let d_ore_id = next_id();
    let e_ore_id = next_id();
    let f_ore_id = next_id();
    let c_module_id = next_id();
    let d_module_id = next_id();
    let e_module_id = next_id();
    let f_module_id = next_id();
    let b_item_id = next_id();
    let b_cont_id = next_id();
    let d_item_id = next_id();
    let d_cont_id = next_id();
    let e_item_id = next_id();
    let e_cont_id = next_id();
    let f_item_id = next_id();
    let f_cont_id = next_id();
    let b_order_id = next_id();
    let d_order_id = next_id();
    let e_order_id = next_id();
    let f_order_id = next_id();
    let b_contract_id = next_id();
    let d_contract_id = next_id();
    let e_contract_id = next_id();
    let f_contract_id = next_id();
    let b_job_id = next_id();
    let corp_a_job_id = next_id();
    let d_job_id = next_id();
    let e_job_id = next_id();
    let f_job_id = next_id();
    let b_blueprint_id = next_id();
    let corp_a_blueprint_id = next_id();
    let d_blueprint_id = next_id();
    let e_blueprint_id = next_id();
    let f_blueprint_id = next_id();
    let b_fitting_id = next_id();
    let d_fitting_id = next_id();
    let e_fitting_id = next_id();
    let f_fitting_id = next_id();
    let b_mail_id = next_id();
    let d_mail_id = next_id();
    let e_mail_id = next_id();
    let f_mail_id = next_id();
    let b_bookmark_id = next_id();
    let d_bookmark_id = next_id();
    let e_bookmark_id = next_id();
    let f_bookmark_id = next_id();
    let b_fleet_id = next_id();
    let d_fleet_id = next_id();
    let e_fleet_id = next_id();
    let f_fleet_id = next_id();
    let b_war_id = next_id();
    let d_war_id = next_id();
    let e_war_id = next_id();
    let f_war_id = next_id();
    let b_killmail_id = next_id();
    let d_killmail_id = next_id();
    let e_killmail_id = next_id();
    let f_killmail_id = next_id();
    let b_pos_id = next_id();
    let d_pos_id = next_id();
    let e_pos_id = next_id();
    let f_pos_id = next_id();
    let b_corp_project_id = next_id();
    let corp_a_project_id = next_id();
    let d_corp_project_id = next_id();
    let e_corp_project_id = next_id();
    let f_corp_project_id = next_id();

    let ship_ids = [
        (CHAR_A_ID, a_ship_id, NPC_STATION_ID),
        (CHAR_B_ID, b_ship_id, NPC_STATION_ID),
        (CHAR_C_ID, c_ship_id, NPC_STATION_ID),
        (CHAR_D_ID, d_ship_id, NPC_STATION_ID),
        (CHAR_E_ID, e_ship_id, other_station_id),
        (CHAR_F_ID, f_ship_id, NPC_STATION_ID),
        (CHAR_A_CEO_ID, ceo_ship_id, NPC_STATION_ID),
    ];

    ctx.db
        .try_with_mut_snapshot(|snap| {
            // Corporations (player corps >= 98_000_000)
            let mut corp_a = CorporationRecord::new(CORP_A_ID, "Neg Corp A", "NCA", CHAR_A_CEO_ID)
                .with_master_wallet(IskCents(100_000_000_00));
            corp_a.members.insert(CHAR_A_CEO_ID);
            corp_a.members.insert(CHAR_C_ID);
            corp_a.member_roles.insert(CHAR_A_CEO_ID, u64::MAX);
            corp_a.member_roles.insert(CHAR_C_ID, 0);
            corp_a.hq_station_id = Some(NPC_STATION_ID);
            snap.corporations.insert(CORP_A_ID, corp_a);

            for (corp_id, name, ticker, ceo_id, hq_st_id, alliance_opt) in [
                (CORP_B_ID, "Neg Corp B", "NCB", CHAR_B_ID, NPC_STATION_ID, Some(ALLIANCE_B_ID)),
                (CORP_D_ID, "Neg Corp D", "NCD", CHAR_D_ID, NPC_STATION_ID, None),
                (CORP_E_ID, "Neg Corp E", "NCE", CHAR_E_ID, other_station_id, None),
                (CORP_F_ID, "Neg Corp F", "NCF", CHAR_F_ID, NPC_STATION_ID, None),
            ] {
                let mut corp = CorporationRecord::new(corp_id, name, ticker, ceo_id)
                    .with_master_wallet(IskCents(100_000_000_00));
                corp.alliance_id = alliance_opt;
                corp.members.insert(ceo_id);
                corp.member_roles.insert(ceo_id, u64::MAX);
                corp.hq_station_id = Some(hq_st_id);
                snap.corporations.insert(corp_id, corp);
            }

            snap.alliances.insert(
                ALLIANCE_B_ID,
                AllianceRecord {
                    alliance_id: ALLIANCE_B_ID,
                    name: "Neg Alliance B".into(),
                    short_name: "NAB".into(),
                    executor_corp_id: CORP_B_ID,
                    member_corp_ids: [CORP_B_ID].into_iter().collect(),
                },
            );

            // Accounts & characters
            for (uname, uid, cid, cname, corp_id, st_id, sys_id, con_id, reg_id) in actor_specs {
                let hashed = WorldRepository::hash_password_token(uname, "pw");
                snap.accounts.insert(
                    uname.to_string(),
                    eve_db::types::AccountRecord {
                        user_id: uid,
                        username: uname.to_string(),
                        password_token: hashed,
                        role: service_roles::ROLE_PLAYER_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
                let my_ship_id = ship_ids
                    .iter()
                    .find(|x| x.0 == cid)
                    .map(|x| x.1)
                    .unwrap();
                snap.characters.insert(
                    cid,
                    CharacterRecord {
                        character_id: cid,
                        user_id: uid,
                        name: cname.to_string(),
                        corporation_id: corp_id,
                        alliance_id: (corp_id == CORP_B_ID).then_some(ALLIANCE_B_ID),
                        station_id: st_id,
                        solar_system_id: sys_id,
                        constellation_id: con_id,
                        region_id: reg_id,
                        active_ship_id: my_ship_id,
                        balance: IskCents(100_000_000_00),
                        home_station_id: st_id.unwrap_or(NPC_STATION_ID),
                        race_id: 1,
                        bloodline_id: 1,
                        ancestry_id: 1,
                        school_id: 11,
                        clone_grade: 2,
                        fleet_id: match cid {
                            CHAR_B_ID => Some(b_fleet_id),
                            CHAR_D_ID => Some(d_fleet_id),
                            CHAR_E_ID => Some(e_fleet_id),
                            CHAR_F_ID => Some(f_fleet_id),
                            _ => None,
                        },
                        version: 1,
                        ..CharacterRecord::default()
                    },
                );
                let mut skills = HashMap::new();
                for sk in [3327_u32, 3329, 3330, 3385, 3389, 3413] {
                    skills.insert(
                        sk,
                        TrainedSkillRecord {
                            type_id: sk,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
                snap.skills.insert(cid, skills);
            }

            // Active ships (Rifter 587)
            for (owner_id, sid, loc_id) in ship_ids {
                snap.items.insert(
                    sid,
                    InventoryItemRecord {
                        item_id: sid,
                        type_id: 587,
                        owner_id,
                        location_id: loc_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
            }

            // Secondary in-space ships, personal containers, cargo items, ores, modules, and blueprints
            for (ship_id, owner_id, sys_loc) in [
                (b_space_ship_id, CHAR_B_ID, SYS_JITA as i64),
                (d_space_ship_id, CHAR_D_ID, SYS_JITA as i64),
                (e_space_ship_id, CHAR_E_ID, other_sys_id as i64),
                (f_space_ship_id, CHAR_F_ID, SYS_JITA as i64),
            ] {
                snap.items.insert(
                    ship_id,
                    InventoryItemRecord {
                        item_id: ship_id,
                        type_id: 587,
                        owner_id,
                        location_id: sys_loc,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
            }
            for (cont_id, item_id, owner_id, st_loc) in [
                (b_cont_id, b_item_id, CHAR_B_ID, NPC_STATION_ID),
                (d_cont_id, d_item_id, CHAR_D_ID, NPC_STATION_ID),
                (e_cont_id, e_item_id, CHAR_E_ID, other_station_id),
                (f_cont_id, f_item_id, CHAR_F_ID, NPC_STATION_ID),
            ] {
                snap.items.insert(
                    cont_id,
                    InventoryItemRecord {
                        item_id: cont_id,
                        type_id: 17366, // Station Container
                        owner_id,
                        location_id: st_loc,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: 34, // Tritanium
                        owner_id,
                        location_id: st_loc,
                        flag: InventoryFlag::Hangar,
                        quantity: 1_000,
                        singleton: false,
                        ..InventoryItemRecord::default()
                    },
                );
            }

            // Reprocessable Veldspar (1230) in personal hangars + fitted Civilian Gatling Railgun (3634) + SDE blueprints
            for (item_id, owner_id, st_loc) in [
                (a_ore_id, CHAR_A_ID, NPC_STATION_ID),
                (c_ore_id, CHAR_C_ID, NPC_STATION_ID),
                (d_ore_id, CHAR_D_ID, NPC_STATION_ID),
                (e_ore_id, CHAR_E_ID, other_station_id),
                (f_ore_id, CHAR_F_ID, NPC_STATION_ID),
            ] {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: 1230, // Veldspar (portion 100)
                        owner_id,
                        location_id: st_loc,
                        flag: InventoryFlag::Hangar,
                        quantity: 500,
                        singleton: false,
                        ..InventoryItemRecord::default()
                    },
                );
            }
            for (mod_id, owner_id, ship_loc) in [
                (c_module_id, CHAR_C_ID, c_ship_id),
                (d_module_id, CHAR_D_ID, d_ship_id),
                (e_module_id, CHAR_E_ID, e_ship_id),
                (f_module_id, CHAR_F_ID, f_ship_id),
            ] {
                snap.items.insert(
                    mod_id,
                    InventoryItemRecord {
                        item_id: mod_id,
                        type_id: 3634,
                        owner_id,
                        location_id: ship_loc,
                        flag: InventoryFlag::HiSlot0,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
            }
            for (bp_id, owner_id, loc_id, flag) in [
                (b_blueprint_id, CHAR_B_ID, NPC_STATION_ID, InventoryFlag::Hangar),
                (corp_a_blueprint_id, CORP_A_ID, CORP_A_OFFICE_ID, InventoryFlag::CorpSAG2),
                (d_blueprint_id, CHAR_D_ID, NPC_STATION_ID, InventoryFlag::Hangar),
                (e_blueprint_id, CHAR_E_ID, other_station_id, InventoryFlag::Hangar),
                (f_blueprint_id, CHAR_F_ID, NPC_STATION_ID, InventoryFlag::Hangar),
            ] {
                snap.items.insert(
                    bp_id,
                    InventoryItemRecord {
                        item_id: bp_id,
                        type_id: bp_type_id,
                        owner_id,
                        location_id: loc_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
            }

            // Corp offices & structures for Corp A (roleless C target), Corp B (A target), and Corp D/E/F (self-owned)
            for (office_id, item_id, corp_id, office_st_id, struct_id, struct_name, pos_x) in [
                (
                    CORP_A_OFFICE_ID,
                    CORP_A_OFFICE_ITEM_ID,
                    CORP_A_ID,
                    NPC_STATION_ID,
                    CORP_A_STRUCTURE_ID,
                    "Neg Structure A",
                    120_000.0,
                ),
                (
                    CORP_B_OFFICE_ID,
                    CORP_B_OFFICE_ITEM_ID,
                    CORP_B_ID,
                    NPC_STATION_ID,
                    CORP_B_STRUCTURE_ID,
                    "Neg Structure B",
                    -120_000.0,
                ),
                (
                    CORP_D_OFFICE_ID,
                    CORP_D_OFFICE_ITEM_ID,
                    CORP_D_ID,
                    NPC_STATION_ID,
                    CORP_D_STRUCTURE_ID,
                    "Neg Structure D",
                    180_000.0,
                ),
                (
                    CORP_E_OFFICE_ID,
                    CORP_E_OFFICE_ITEM_ID,
                    CORP_E_ID,
                    other_station_id,
                    CORP_E_STRUCTURE_ID,
                    "Neg Structure E",
                    -180_000.0,
                ),
                (
                    CORP_F_OFFICE_ID,
                    CORP_F_OFFICE_ITEM_ID,
                    CORP_F_ID,
                    NPC_STATION_ID,
                    CORP_F_STRUCTURE_ID,
                    "Neg Structure F",
                    350_000.0,
                ),
            ] {
                snap.items.insert(
                    office_id,
                    InventoryItemRecord {
                        item_id: office_id,
                        type_id: 27, // Office
                        owner_id: corp_id,
                        location_id: office_st_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
                let mut gov = CorpGovState::default();
                gov.offices.insert(office_st_id, office_id);
                snap.corp_governance.insert(corp_id, gov);

                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: 34,
                        owner_id: corp_id,
                        location_id: office_id,
                        flag: InventoryFlag::CorpSAG2,
                        quantity: 500,
                        singleton: false,
                        ..InventoryItemRecord::default()
                    },
                );

                snap.structures.insert(
                    struct_id,
                    UpwellStructureRecord {
                        structure_id: struct_id,
                        type_id: 35835, // Athanor
                        owner_corp_id: corp_id,
                        solar_system_id: SYS_JITA,
                        name: struct_name.to_string(),
                        position: [pos_x, 0.0, 0.0],
                        state: "ShieldVulnerable".to_string(),
                        quantum_core_installed: true,
                        fuel_blocks: 500,
                        ..UpwellStructureRecord::default()
                    },
                );
                snap.items.insert(
                    struct_id,
                    InventoryItemRecord {
                        item_id: struct_id,
                        type_id: 35835,
                        owner_id: corp_id,
                        location_id: SYS_JITA as i64,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
            }

            // Per-actor POS tower, market order, contract, industry job, fitting, mail, bookmark, fleet, war, killmail, planet, corp project
            for (
                char_id,
                corp_id,
                st_id,
                sys_id,
                region_id,
                pos_id,
                order_id,
                contract_id,
                job_id,
                bp_id,
                fitting_id,
                mail_id,
                bookmark_id,
                fleet_id,
                war_id,
                killmail_id,
                planet_id,
                corp_project_id,
            ) in [
                (
                    CHAR_B_ID,
                    CORP_B_ID,
                    NPC_STATION_ID,
                    SYS_JITA,
                    jita_region,
                    b_pos_id,
                    b_order_id,
                    b_contract_id,
                    b_job_id,
                    b_blueprint_id,
                    b_fitting_id,
                    b_mail_id,
                    b_bookmark_id,
                    b_fleet_id,
                    b_war_id,
                    b_killmail_id,
                    b_planet_id,
                    b_corp_project_id,
                ),
                (
                    CHAR_D_ID,
                    CORP_D_ID,
                    NPC_STATION_ID,
                    SYS_JITA,
                    jita_region,
                    d_pos_id,
                    d_order_id,
                    d_contract_id,
                    d_job_id,
                    d_blueprint_id,
                    d_fitting_id,
                    d_mail_id,
                    d_bookmark_id,
                    d_fleet_id,
                    d_war_id,
                    d_killmail_id,
                    d_planet_id,
                    d_corp_project_id,
                ),
                (
                    CHAR_E_ID,
                    CORP_E_ID,
                    other_station_id,
                    other_sys_id,
                    other_region,
                    e_pos_id,
                    e_order_id,
                    e_contract_id,
                    e_job_id,
                    e_blueprint_id,
                    e_fitting_id,
                    e_mail_id,
                    e_bookmark_id,
                    e_fleet_id,
                    e_war_id,
                    e_killmail_id,
                    e_planet_id,
                    e_corp_project_id,
                ),
                (
                    CHAR_F_ID,
                    CORP_F_ID,
                    NPC_STATION_ID,
                    SYS_JITA,
                    jita_region,
                    f_pos_id,
                    f_order_id,
                    f_contract_id,
                    f_job_id,
                    f_blueprint_id,
                    f_fitting_id,
                    f_mail_id,
                    f_bookmark_id,
                    f_fleet_id,
                    f_war_id,
                    f_killmail_id,
                    f_planet_id,
                    f_corp_project_id,
                ),
            ] {
                snap.items.insert(
                    pos_id,
                    InventoryItemRecord {
                        item_id: pos_id,
                        type_id: 12235,
                        owner_id: corp_id,
                        location_id: SYS_JITA as i64,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        ..InventoryItemRecord::default()
                    },
                );
                snap.market_orders.insert(
                    order_id,
                    MarketOrderRecord {
                        order_id,
                        owner_id: char_id,
                        station_id: st_id,
                        region_id,
                        type_id: 34,
                        is_bid: false,
                        price: IskCents(1_000),
                        vol_entered: 100,
                        vol_remaining: 100,
                        min_volume: 1,
                        is_npc: false,
                        escrow: IskCents(0),
                    },
                );
                snap.contracts.insert(
                    contract_id,
                    ContractRecord {
                        contract_id,
                        issuer_id: char_id,
                        issuer_corp_id: corp_id,
                        contract_type: "ItemExchange".into(),
                        status: "Outstanding".into(),
                        start_station_id: st_id,
                        end_station_id: st_id,
                        price: IskCents(50_000_00),
                        availability: "Personal".into(),
                        assignee_id: char_id,
                        expiration_unix: 2_000_000_000,
                        ..ContractRecord::default()
                    },
                );
                snap.industry_jobs.insert(
                    job_id,
                    IndustryJobRecord {
                        job_id,
                        installer_id: char_id,
                        facility_id: st_id,
                        blueprint_item_id: bp_id,
                        blueprint_type_id: bp_type_id,
                        activity_id: 1,
                        runs: 1,
                        product_type_id: 587,
                        status: "Active".into(),
                        duration_seconds: 3600,
                        blueprint_location_id: st_id,
                        output_location_id: st_id,
                        ..IndustryJobRecord::default()
                    },
                );
                snap.saved_fittings.insert(
                    fitting_id,
                    SavedFittingRecord {
                        fitting_id,
                        owner_id: char_id,
                        name: format!("Fit {char_id}"),
                        ship_type_id: 587,
                        description: "".into(),
                        items: vec![],
                    },
                );
                snap.mails.push(MailRecord {
                    message_id: mail_id,
                    sender_id: char_id,
                    recipient_id: char_id,
                    subject: "Secret".into(),
                    body: "Private mail".into(),
                });
                snap.bookmarks.insert(
                    bookmark_id,
                    BookmarkRecord {
                        bookmark_id,
                        owner_id: char_id,
                        label: format!("Safe {char_id}"),
                        solar_system_id: sys_id,
                        x: 1000.0,
                        y: 0.0,
                        z: 0.0,
                        item_id: Some(st_id),
                        memo: String::new(),
                        folder_id: None,
                        is_shared: false,
                    },
                );
                snap.fleets.insert(
                    fleet_id,
                    FleetRecord {
                        fleet_id,
                        boss_id: char_id,
                        members: vec![char_id],
                        member_details: [(
                            char_id,
                            FleetMemberRecord {
                                character_id: char_id,
                                role: FleetRole::FleetCommander,
                                ..FleetMemberRecord::default()
                            },
                        )]
                        .into_iter()
                        .collect(),
                        ..FleetRecord::default()
                    },
                );
                snap.wars.insert(
                    war_id,
                    WarRecord {
                        war_id,
                        aggressor_corp_id: corp_id,
                        defender_corp_id: 1_000_009,
                        active: true,
                        ..WarRecord::default()
                    },
                );
                snap.killmails.insert(
                    killmail_id,
                    KillmailRecord {
                        kill_id: killmail_id,
                        victim_character_id: char_id,
                        attacker_character_id: 9_000_001,
                        solar_system_id: sys_id,
                        ship_type_id: 587,
                        total_value_isk: 500_000.0,
                        killed_unix: 1_700_000_000,
                        victim_corp_id: corp_id,
                        attacker_corp_id: 98_099_099,
                        ..KillmailRecord::default()
                    },
                );
                snap.planetary_colonies.insert(
                    (char_id, planet_id),
                    PlanetaryColonyRecord {
                        planet_id,
                        owner_id: char_id,
                        solar_system_id: sys_id,
                        command_center_level: 1,
                        pins_count: 1,
                        ..PlanetaryColonyRecord::default()
                    },
                );
                snap.corp_projects.insert(
                    corp_project_id,
                    CorpProjectRecord {
                        project_id: corp_project_id,
                        corporation_id: corp_id,
                        name: format!("Corp Project {corp_id}"),
                        target_quantity: 100,
                        ..Default::default()
                    },
                );
            }

            // Corp A corp-owned industry job & project (Situation C targets)
            snap.industry_jobs.insert(
                corp_a_job_id,
                IndustryJobRecord {
                    job_id: corp_a_job_id,
                    installer_id: CHAR_A_CEO_ID,
                    facility_id: CORP_A_STRUCTURE_ID,
                    blueprint_item_id: corp_a_blueprint_id,
                    blueprint_type_id: bp_type_id,
                    activity_id: 1,
                    runs: 1,
                    product_type_id: 587,
                    status: "Active".into(),
                    duration_seconds: 3600,
                    blueprint_location_id: CORP_A_OFFICE_ID,
                    output_location_id: CORP_A_OFFICE_ID,
                    ..IndustryJobRecord::default()
                },
            );
            save_job_cost_meta(
                snap,
                corp_a_job_id,
                &IndustryJobCostMeta {
                    use_corp: true,
                    owner_id: CORP_A_ID,
                    ..Default::default()
                },
            );
            snap.corp_projects.insert(
                corp_a_project_id,
                CorpProjectRecord {
                    project_id: corp_a_project_id,
                    corporation_id: CORP_A_ID,
                    name: "Corp Project".into(),
                    target_quantity: 100,
                    ..Default::default()
                },
            );

            snap.next_entity_id = snap.next_entity_id.max(alloc + 10_000);
            Ok::<_, std::convert::Infallible>(())
        })
        .expect("seed negative matrix world");

    let (jita_gate_opt, jita_gate_pos_opt, _, _) = find_system_gate_and_station(&ctx, SYS_JITA);
    let jita_gate_id = jita_gate_opt.expect("Jita stargate present");
    let jita_gate_pos = jita_gate_pos_opt.expect("Jita gate position");

    let (other_gate_opt, _, other_st_opt, other_st_pos_opt) =
        find_system_gate_and_station(&ctx, other_sys_id);
    let other_gate_id = other_gate_opt.unwrap_or(other_station_id);
    let _other_st = other_st_opt.unwrap_or(other_station_id);
    let other_st_pos = other_st_pos_opt.unwrap_or(DVec3::ZERO);

    // Log in A..F cleanly over wire and select characters
    let kw = HashMap::new();
    let mut sess_a = SessionState::new(901_001);
    let mut sess_b = SessionState::new(901_002);
    let mut sess_c = SessionState::new(901_003);
    let mut sess_d = SessionState::new(901_004);
    let mut sess_e = SessionState::new(901_005);
    let mut sess_f = SessionState::new(901_006);

    for (sess, uname, cid) in [
        (&mut sess_a, "neg_a", CHAR_A_ID),
        (&mut sess_b, "neg_b", CHAR_B_ID),
        (&mut sess_c, "neg_c", CHAR_C_ID),
        (&mut sess_d, "neg_d", CHAR_D_ID),
        (&mut sess_e, "neg_e", CHAR_E_ID),
        (&mut sess_f, "neg_f", CHAR_F_ID),
    ] {
        common::login_pilot(&ctx, sess, uname, "pw").await;
        let mut h =
            common::borrow_wire_harness(&ctx, sess).with_mode(ContractValidationMode::ReportOnly);
        let sel = h
            .call_exchange(
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(cid)],
                &kw,
            )
            .await;
        assert!(sel.result.is_ok(), "SelectCharacterID failed for {cid}: {:?}", sel.result);
    }

    // Seed per-actor petitions over the wire before undocking / notification reset
    let mut b_petition_id = 0i64;
    let mut c_petition_id = 0i64;
    let mut d_petition_id = 0i64;
    let mut e_petition_id = 0i64;
    let mut f_petition_id = 0i64;
    for (sess, pet_out) in [
        (&mut sess_b, &mut b_petition_id),
        (&mut sess_c, &mut c_petition_id),
        (&mut sess_d, &mut d_petition_id),
        (&mut sess_e, &mut e_petition_id),
        (&mut sess_f, &mut f_petition_id),
    ] {
        let mut h =
            common::borrow_wire_harness(&ctx, sess).with_mode(ContractValidationMode::ReportOnly);
        let res = h
            .call_exchange(
                "petitioner",
                None,
                "CreatePetition",
                &[
                    PyRep::String("Neg Support Ticket".into()),
                    PyRep::String("Negative matrix seeded petition".into()),
                    PyRep::Integer(1),
                ],
                &kw,
            )
            .await;
        *pet_out = res
            .result
            .expect("CreatePetition should succeed")
            .dict_get("petitionID")
            .and_then(PyRep::as_i64)
            .expect("petitionID in CreatePetition result");
    }

    // Undock E (into SYS_OTHER) and F (into SYS_JITA) via wire
    ctx.advance_secs(31);
    {
        let mut h = common::borrow_wire_harness(&ctx, &mut sess_e)
            .with_mode(ContractValidationMode::ReportOnly);
        let out = h.call_exchange("stationSvc", None, "Undock", &[], &kw).await;
        assert!(out.result.is_ok(), "Undock E failed: {:?}", out.result);
    }
    {
        let mut h = common::borrow_wire_harness(&ctx, &mut sess_f)
            .with_mode(ContractValidationMode::ReportOnly);
        let out = h.call_exchange("stationSvc", None, "Undock", &[], &kw).await;
        assert!(out.result.is_ok(), "Undock F failed: {:?}", out.result);
    }
    ctx.advance_secs(31);
    ctx.session_change_timers.lock().expect("timers").clear();
    ctx.systems.restore_location_change_invuln(Default::default());

    let make_ball = |id: i64, type_id: u32, position: DVec3| Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 30.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };

    // Position E 500 km from SYS_OTHER station (where e_space_ship_id sits) and initialize combat baseline.
    let e_ship_pos = other_st_pos + DVec3::new(500_000.0, 0.0, 0.0);
    let sys_other = ctx.systems.get_or_spawn(other_sys_id);
    assert!(
        sys_other.set_ball_position(e_ship_id, e_ship_pos).await,
        "set E position"
    );
    {
        let mut bp = sys_other.ballpark.write().expect("write other bp");
        bp.balls
            .insert(e_space_ship_id, make_ball(e_space_ship_id, 587, other_st_pos));
        bp.combat_states
            .insert(e_space_ship_id, CombatState::default());
    }
    let _ = sys_other.set_warp_core_strength(e_ship_id, 1).await;

    // Place Corp F's Upwell structure > 250 km from F's gate position before tackle reconciliation.
    let f_struct_pos = jita_gate_pos + DVec3::new(350_000.0, 0.0, 0.0);
    ctx.db
        .try_with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&CORP_F_STRUCTURE_ID) {
                st.position = [f_struct_pos.x, f_struct_pos.y, f_struct_pos.z];
            }
            Ok::<_, std::convert::Infallible>(())
        })
        .expect("position Corp F structure");

    // Seed space balls in Jita BEFORE scramble reconciliation so baseline combat_states already reflect tackle reconciliation.
    let sys_jita = ctx.systems.get_or_spawn(SYS_JITA);
    {
        let mut bp = sys_jita.ballpark.write().expect("write jita bp");
        if let Some(st_ball) = bp.balls.get_mut(&CORP_F_STRUCTURE_ID) {
            st_ball.position = f_struct_pos;
        }
        for (ball_id, type_id, pos) in [
            (b_space_ship_id, 587u32, jita_gate_pos + DVec3::new(10_000.0, 0.0, 0.0)),
            (d_space_ship_id, 587, jita_gate_pos + DVec3::new(20_000.0, 0.0, 0.0)),
            (f_space_ship_id, 587, jita_gate_pos + DVec3::new(300_000.0, 0.0, 0.0)),
            (f_pos_id, 12235, f_struct_pos),
        ] {
            bp.balls.insert(ball_id, make_ball(ball_id, type_id, pos));
            bp.combat_states.insert(ball_id, CombatState::default());
        }
    }

    // Position F 1.2 km from Jita gate and scram + cloak it.
    let f_ship_pos = jita_gate_pos + DVec3::new(1_200.0, 0.0, 0.0);
    assert!(
        sys_jita.set_ball_position(f_ship_id, f_ship_pos).await,
        "set F position"
    );
    let _ = sys_jita.set_warp_scramble_strength(f_ship_id, 5).await;
    let _ = sys_jita
        .modify_combat_state(f_ship_id, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
            cs.gate_cloak_remaining_ticks = 0;
            cs.undock_invulnerable_remaining_ticks = 0;
        })
        .await;

    sess_b.fleet_id = Some(b_fleet_id);
    sess_d.fleet_id = Some(d_fleet_id);
    sess_e.fleet_id = Some(e_fleet_id);
    sess_f.fleet_id = Some(f_fleet_id);

    // Bind legitimate own objects per actor B..F
    let bind_specs_for = |sys_id: SolarSystemId,
                          st_id: i64,
                          cid: i64,
                          corp_id: i64,
                          ship_id: i64,
                          fleet_opt: Option<i64>,
                          planet_id: i64,
                          struct_id: i64,
                          pos_id: i64,
                          war_id: i64|
     -> Vec<(&'static str, i64)> {
        let mut specs = vec![
            ("beyonce", sys_id as i64),
            ("scanMgr", sys_id as i64),
            ("entity", sys_id as i64),
            ("ship", ship_id),
            ("dogmaIM", ship_id),
            ("invbroker", st_id),
            ("stationSvc", st_id),
            ("station", st_id),
            ("corpStationMgr", st_id),
            ("repairSvc", st_id),
            ("reprocessingSvc", st_id),
            ("insuranceSvc", st_id),
            ("officeManager", st_id),
            ("skillMgr2", cid),
            ("charMgr", cid),
            ("jumpCloneSvc", cid),
            ("contractProxy", cid),
            ("populationCap", cid),
            ("crimewatch", ship_id),
            ("corpRegistry", corp_id),
            ("corpmgr", corp_id),
            ("voucher", cid),
            ("agentMgr", 3_018_600),
            ("devIndexManager", sys_id as i64),
            ("planetOrbitalRegistryBroker", sys_id as i64),
        ];
        if let Some(fleet_id) = fleet_opt {
            specs.push(("fleetObjectHandler", fleet_id));
            specs.push(("planetMgr", planet_id));
            specs.push(("structure", struct_id));
            specs.push(("posMgr", pos_id));
            specs.push(("warRegistry", corp_id));
            specs.push(("warStatisticMgr", war_id));
        }
        specs
    };

    let mut actor_bounds: [HashMap<String, u64>; 6] = Default::default();
    let actor_setup = [
        (1usize, SYS_JITA, NPC_STATION_ID, CHAR_A_ID, 1_000_009i64, a_ship_id, None, 0i64, 0i64, 0i64, 0i64),
        (2, SYS_JITA, NPC_STATION_ID, CHAR_C_ID, CORP_A_ID, c_ship_id, None, 0, 0, 0, 0),
        (3, SYS_JITA, NPC_STATION_ID, CHAR_D_ID, CORP_D_ID, d_ship_id, Some(d_fleet_id), d_planet_id, CORP_D_STRUCTURE_ID, d_pos_id, d_war_id),
        (4, other_sys_id, 0, CHAR_E_ID, CORP_E_ID, e_ship_id, Some(e_fleet_id), e_planet_id, CORP_E_STRUCTURE_ID, e_pos_id, e_war_id),
        (5, SYS_JITA, 0, CHAR_F_ID, CORP_F_ID, f_ship_id, Some(f_fleet_id), f_planet_id, CORP_F_STRUCTURE_ID, f_pos_id, f_war_id),
    ];

    let sess_a_ptr = &mut sess_a;
    let sess_c_ptr = &mut sess_c;
    let sess_d_ptr = &mut sess_d;
    let sess_e_ptr = &mut sess_e;
    let sess_f_ptr = &mut sess_f;
    let sess_muts = [sess_a_ptr, sess_c_ptr, sess_d_ptr, sess_e_ptr, sess_f_ptr];

    for ((idx, sys_id, st_id, cid, corp_id, sid, fleet_opt, planet_id, struct_id, pos_id, war_id), sess_ref) in
        actor_setup.into_iter().zip(sess_muts.into_iter())
    {
        for (svc, obj_id) in bind_specs_for(
            sys_id, st_id, cid, corp_id, sid, fleet_opt, planet_id, struct_id, pos_id, war_id,
        ) {
            if obj_id > 0 {
                if let Some(bid) = bind_service_for_session(&ctx, sess_ref, svc, obj_id).await {
                    actor_bounds[idx].insert(svc.to_string(), bid);
                }
            }
        }
    }

    // Create B's bound objects across all 36 bindable services
    let b_bind_specs: Vec<(&'static str, i64)> = vec![
        ("populationCap", CHAR_B_ID),
        ("beyonce", SYS_JITA as i64),
        ("scanMgr", SYS_JITA as i64),
        ("entity", SYS_JITA as i64),
        ("ship", b_ship_id),
        ("dogmaIM", b_ship_id),
        ("invbroker", NPC_STATION_ID),
        ("station", NPC_STATION_ID),
        ("stationSvc", NPC_STATION_ID),
        ("corpStationMgr", NPC_STATION_ID),
        ("repairSvc", NPC_STATION_ID),
        ("reprocessingSvc", NPC_STATION_ID),
        ("insuranceSvc", NPC_STATION_ID),
        ("skillMgr2", CHAR_B_ID),
        ("charMgr", CHAR_B_ID),
        ("jumpCloneSvc", CHAR_B_ID),
        ("crimewatch", b_ship_id),
        ("corpRegistry", CORP_B_ID),
        ("corpmgr", CORP_B_ID),
        ("officeManager", NPC_STATION_ID),
        ("allianceRegistry", ALLIANCE_B_ID),
        ("fleetObjectHandler", b_fleet_id),
        ("planetMgr", b_planet_id),
        ("planetOrbitalRegistryBroker", SYS_JITA as i64),
        ("structure", CORP_B_STRUCTURE_ID),
        ("posMgr", b_pos_id),
        ("warRegistry", CORP_B_ID),
        ("warStatisticMgr", b_war_id),
        ("devIndexManager", SYS_JITA as i64),
        ("voucher", CHAR_B_ID),
        ("contractProxy", CHAR_B_ID),
        ("agentMgr", 3_018_600),
    ];

    let mut b_bounds = Vec::new();
    for (svc, obj_id) in b_bind_specs {
        if let Some(bid) = bind_service_for_session(&ctx, &mut sess_b, svc, obj_id).await {
            b_bounds.push((svc.to_string(), bid));
        }
    }
    // Add synthetic B-owned BoundObjectEntry records for remaining specialized bindable services (`i2`, `keeper`, `tourneyMgr`, `trademgr`)
    let mut synthetic_bid = 9_800_000u64;
    let bound_set: HashSet<String> = b_bounds.iter().map(|(s, _)| s.clone()).collect();
    for svc in ["i2", "keeper", "tourneyMgr", "trademgr"] {
        if !bound_set.contains(svc) {
            synthetic_bid += 1;
            ctx.bound_objects.write().expect("write bound_objects").insert(
                synthetic_bid,
                BoundObjectEntry {
                    bound_id: synthetic_bid,
                    session_id: sess_b.session_id,
                    user_id: USER_B_ID,
                    character_id: CHAR_B_ID,
                    service: svc.to_string(),
                    object_id: b_ship_id,
                    oid_str: format!("N=1:{synthetic_bid}"),
                    ref_id: synthetic_bid as i64,
                },
            );
            b_bounds.push((svc.to_string(), synthetic_bid));
        }
    }

    ctx.pending_notifications.write().expect("clear notif").clear();
    ctx.notification_records.write().expect("clear recs").clear();

    let sess_by_sit = [
        SessionState::new(901_000), // A: not logged in
        sess_a,
        sess_c,
        sess_d,
        sess_e,
        sess_f,
    ];

    MatrixFixture {
        ctx,
        _bug_dir: bug_dir,
        other_sys_id,
        other_station_id,
        non_hq_station_id,
        other_gate_id,
        jita_gate_id,
        a_ship_id,
        b_ship_id,
        b_space_ship_id,
        c_ship_id,
        d_ship_id,
        d_space_ship_id,
        e_ship_id,
        e_space_ship_id,
        f_ship_id,
        f_space_ship_id,
        a_ore_id,
        c_ore_id,
        d_ore_id,
        e_ore_id,
        f_ore_id,
        c_module_id,
        d_module_id,
        e_module_id,
        f_module_id,
        b_item_id,
        b_cont_id,
        d_item_id,
        d_cont_id,
        e_item_id,
        e_cont_id,
        f_item_id,
        f_cont_id,
        b_order_id,
        d_order_id,
        e_order_id,
        f_order_id,
        b_contract_id,
        d_contract_id,
        e_contract_id,
        f_contract_id,
        b_job_id,
        corp_a_job_id,
        d_job_id,
        e_job_id,
        f_job_id,
        b_blueprint_id,
        corp_a_blueprint_id,
        d_blueprint_id,
        e_blueprint_id,
        f_blueprint_id,
        b_fitting_id,
        d_fitting_id,
        e_fitting_id,
        f_fitting_id,
        b_mail_id,
        d_mail_id,
        e_mail_id,
        f_mail_id,
        b_bookmark_id,
        d_bookmark_id,
        e_bookmark_id,
        f_bookmark_id,
        b_fleet_id,
        d_fleet_id,
        e_fleet_id,
        f_fleet_id,
        b_war_id,
        d_war_id,
        e_war_id,
        f_war_id,
        b_killmail_id,
        d_killmail_id,
        e_killmail_id,
        f_killmail_id,
        b_pos_id,
        d_pos_id,
        e_pos_id,
        f_pos_id,
        b_planet_id,
        d_planet_id,
        e_planet_id,
        f_planet_id,
        b_corp_project_id,
        corp_a_project_id,
        d_corp_project_id,
        e_corp_project_id,
        f_corp_project_id,
        b_petition_id,
        c_petition_id,
        d_petition_id,
        e_petition_id,
        f_petition_id,
        sess_by_sit,
        actor_bounds,
        b_bounds,
    }
}

// ============================================================================
// Realistic Per-Situation Argument Matrix
// ============================================================================

fn build_call_args(
    fix: &MatrixFixture,
    sit: Situation,
    svc: &str,
    meth: &str,
) -> (Option<u64>, Vec<PyRep>) {
    let idx = sit as usize;
    let own_char = match sit {
        Situation::A => 0,
        Situation::B => CHAR_A_ID,
        Situation::C => CHAR_C_ID,
        Situation::D => CHAR_D_ID,
        Situation::E => CHAR_E_ID,
        Situation::F => CHAR_F_ID,
    };
    let own_corp = match sit {
        Situation::A => 0,
        Situation::B => 1_000_009,
        Situation::C => CORP_A_ID,
        Situation::D => CORP_D_ID,
        Situation::E => CORP_E_ID,
        Situation::F => CORP_F_ID,
    };
    let own_ship = match sit {
        Situation::A => 0,
        Situation::B => fix.a_ship_id,
        Situation::C => fix.c_ship_id,
        Situation::D => fix.d_ship_id,
        Situation::E => fix.e_ship_id,
        Situation::F => fix.f_ship_id,
    };
    let own_sys = match sit {
        Situation::A | Situation::B | Situation::C | Situation::D | Situation::F => SYS_JITA as i64,
        Situation::E => fix.other_sys_id as i64,
    };
    let own_station = match sit {
        Situation::E => fix.other_station_id,
        _ => NPC_STATION_ID,
    };
    let own_structure = match sit {
        Situation::C => CORP_A_STRUCTURE_ID,
        Situation::D => CORP_D_STRUCTURE_ID,
        Situation::E => CORP_E_STRUCTURE_ID,
        Situation::F => CORP_F_STRUCTURE_ID,
        _ => CORP_B_STRUCTURE_ID,
    };
    let own_office = match sit {
        Situation::C => CORP_A_OFFICE_ID,
        Situation::D => CORP_D_OFFICE_ID,
        Situation::E => CORP_E_OFFICE_ID,
        Situation::F => CORP_F_OFFICE_ID,
        _ => CORP_B_OFFICE_ID,
    };
    let own_corp_item = match sit {
        Situation::C => CORP_A_OFFICE_ITEM_ID,
        Situation::D => CORP_D_OFFICE_ITEM_ID,
        Situation::E => CORP_E_OFFICE_ITEM_ID,
        Situation::F => CORP_F_OFFICE_ITEM_ID,
        _ => CORP_B_OFFICE_ITEM_ID,
    };
    let own_space_ship = match sit {
        Situation::D => fix.d_space_ship_id,
        Situation::E => fix.e_space_ship_id,
        Situation::F => fix.f_space_ship_id,
        _ => fix.b_space_ship_id,
    };
    let own_ore = match sit {
        Situation::C => fix.c_ore_id,
        Situation::D => fix.d_ore_id,
        Situation::E => fix.e_ore_id,
        Situation::F => fix.f_ore_id,
        _ => fix.a_ore_id,
    };
    let own_module = match sit {
        Situation::C => fix.c_module_id,
        Situation::D => fix.d_module_id,
        Situation::E => fix.e_module_id,
        Situation::F => fix.f_module_id,
        _ => fix.b_item_id,
    };
    let own_item = match sit {
        Situation::D => fix.d_item_id,
        Situation::E => fix.e_item_id,
        Situation::F => fix.f_item_id,
        _ => fix.b_item_id,
    };
    let own_cont = match sit {
        Situation::D => fix.d_cont_id,
        Situation::E => fix.e_cont_id,
        Situation::F => fix.f_cont_id,
        _ => fix.b_cont_id,
    };
    let own_order = match sit {
        Situation::D => fix.d_order_id,
        Situation::E => fix.e_order_id,
        Situation::F => fix.f_order_id,
        _ => fix.b_order_id,
    };
    let own_contract = match sit {
        Situation::D => fix.d_contract_id,
        Situation::E => fix.e_contract_id,
        Situation::F => fix.f_contract_id,
        _ => fix.b_contract_id,
    };
    let own_job = match sit {
        Situation::C => fix.corp_a_job_id,
        Situation::D => fix.d_job_id,
        Situation::E => fix.e_job_id,
        Situation::F => fix.f_job_id,
        _ => fix.b_job_id,
    };
    let own_blueprint = match sit {
        Situation::C => fix.corp_a_blueprint_id,
        Situation::D => fix.d_blueprint_id,
        Situation::E => fix.e_blueprint_id,
        Situation::F => fix.f_blueprint_id,
        _ => fix.b_blueprint_id,
    };
    let own_fitting = match sit {
        Situation::D => fix.d_fitting_id,
        Situation::E => fix.e_fitting_id,
        Situation::F => fix.f_fitting_id,
        _ => fix.b_fitting_id,
    };
    let own_mail = match sit {
        Situation::D => fix.d_mail_id,
        Situation::E => fix.e_mail_id,
        Situation::F => fix.f_mail_id,
        _ => fix.b_mail_id,
    };
    let own_bookmark = match sit {
        Situation::D => fix.d_bookmark_id,
        Situation::E => fix.e_bookmark_id,
        Situation::F => fix.f_bookmark_id,
        _ => fix.b_bookmark_id,
    };
    let own_fleet = match sit {
        Situation::D => fix.d_fleet_id,
        Situation::E => fix.e_fleet_id,
        Situation::F => fix.f_fleet_id,
        _ => fix.b_fleet_id,
    };
    let own_war = match sit {
        Situation::D => fix.d_war_id,
        Situation::E => fix.e_war_id,
        Situation::F => fix.f_war_id,
        _ => fix.b_war_id,
    };
    let own_killmail = match sit {
        Situation::D => fix.d_killmail_id,
        Situation::E => fix.e_killmail_id,
        Situation::F => fix.f_killmail_id,
        _ => fix.b_killmail_id,
    };
    let own_pos = match sit {
        Situation::D => fix.d_pos_id,
        Situation::E => fix.e_pos_id,
        Situation::F => fix.f_pos_id,
        _ => fix.b_pos_id,
    };
    let own_planet = match sit {
        Situation::D => fix.d_planet_id,
        Situation::E => fix.e_planet_id,
        Situation::F => fix.f_planet_id,
        _ => fix.b_planet_id,
    };
    let own_corp_project = match sit {
        Situation::C => fix.corp_a_project_id,
        Situation::D => fix.d_corp_project_id,
        Situation::E => fix.e_corp_project_id,
        Situation::F => fix.f_corp_project_id,
        _ => fix.b_corp_project_id,
    };
    let own_petition = match sit {
        Situation::C => fix.c_petition_id,
        Situation::D => fix.d_petition_id,
        Situation::E => fix.e_petition_id,
        Situation::F => fix.f_petition_id,
        _ => fix.b_petition_id,
    };

    if meth == "MachoResolveObject" {
        let tgt = match sit {
            Situation::B => match svc {
                "keeper" => fix.other_sys_id as i64,
                "planetMgr" => fix.e_planet_id,
                _ => fix.b_ship_id,
            },
            Situation::C => CORP_A_ID,
            Situation::E => fix.other_sys_id as i64,
            _ => NPC_STATION_ID,
        };
        return (None, vec![PyRep::Integer(tgt)]);
    }
    if meth == "MachoBindObject" {
        let tgt = match sit {
            Situation::B => match svc {
                "ship" | "dogmaIM" | "crimewatch" | "trademgr" => fix.b_ship_id,
                "invbroker" | "i2" => fix.b_cont_id,
                "corpRegistry" | "corpmgr" | "warRegistry" => CORP_B_ID,
                "allianceRegistry" => ALLIANCE_B_ID,
                "fleetObjectHandler" => fix.b_fleet_id,
                "planetMgr" => fix.b_planet_id,
                "structure" => CORP_B_STRUCTURE_ID,
                "posMgr" => fix.b_pos_id,
                "warStatisticMgr" => fix.b_war_id,
                "charMgr" | "skillMgr2" | "jumpCloneSvc" | "contractProxy" | "voucher"
                | "populationCap" => CHAR_B_ID,
                "beyonce" | "scanMgr" | "entity" | "devIndexManager"
                | "planetOrbitalRegistryBroker" => fix.other_sys_id as i64,
                _ => CORP_B_STRUCTURE_ID,
            },
            Situation::C => match svc {
                "corpRegistry" | "corpmgr" | "warRegistry" | "officeManager" | "voucher" => CORP_A_ID,
                "structure" => CORP_A_STRUCTURE_ID,
                "invbroker" => CORP_A_OFFICE_ID,
                _ => own_char,
            },
            Situation::E | Situation::F => match svc {
                "beyonce" | "scanMgr" | "entity" | "devIndexManager"
                | "planetOrbitalRegistryBroker" => own_sys,
                "ship" | "dogmaIM" | "crimewatch" | "invbroker" => own_ship,
                "corpRegistry" | "corpmgr" | "warRegistry" => own_corp,
                "fleetObjectHandler" => own_fleet,
                "planetMgr" => own_planet,
                "structure" => own_structure,
                "posMgr" => own_pos,
                "warStatisticMgr" => own_war,
                "station" | "stationSvc" => NPC_STATION_ID,
                _ => own_char,
            },
            _ => match svc {
                "beyonce" | "scanMgr" | "entity" | "devIndexManager"
                | "planetOrbitalRegistryBroker" => own_sys,
                "ship" | "dogmaIM" | "crimewatch" => own_ship,
                "corpRegistry" | "corpmgr" | "warRegistry" => own_corp,
                "fleetObjectHandler" => own_fleet,
                "planetMgr" => own_planet,
                "structure" => own_structure,
                "posMgr" => own_pos,
                "warStatisticMgr" => own_war,
                "station" | "stationSvc" | "corpStationMgr" | "repairSvc"
                | "reprocessingSvc" | "insuranceSvc" | "invbroker" | "officeManager" => {
                    NPC_STATION_ID
                }
                _ => own_char,
            },
        };
        return (None, vec![PyRep::Integer(tgt), PyRep::None]);
    }

    let bound_id = if sit == Situation::A {
        None
    } else {
        fix.actor_bounds[idx].get(svc).copied()
    };

    let args = match svc {
        "facilityManager" => match meth {
            "SetFacilityTaxes" => {
                let fac = match sit {
                    Situation::B => NPC_STATION_ID, // #1182 NPC station tax target
                    Situation::C => CORP_A_STRUCTURE_ID, // roleless corp member on own corp structure
                    _ => own_structure,
                };
                vec![
                    PyRep::Integer(fac),
                    PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.05))]),
                ]
            }
            _ => vec![PyRep::Integer(match sit {
                Situation::B => CORP_B_STRUCTURE_ID,
                Situation::C => CORP_A_STRUCTURE_ID,
                _ => own_station,
            })],
        },
        "reprocessingSvc" => match meth {
            "Reprocess" => match sit {
                Situation::B => vec![
                    PyRep::List(vec![PyRep::Integer(fix.a_ore_id)]),
                    PyRep::Integer(NPC_STATION_ID),
                    PyRep::Integer(CHAR_B_ID),
                    PyRep::Integer(fix.b_cont_id),
                    PyRep::Integer(4),
                ],
                Situation::C => vec![
                    PyRep::List(vec![PyRep::Integer(fix.c_ore_id)]),
                    PyRep::Integer(NPC_STATION_ID),
                    PyRep::Integer(CORP_A_ID),
                    PyRep::Integer(CORP_A_OFFICE_ID),
                    PyRep::Integer(117),
                ],
                Situation::D => vec![
                    PyRep::List(vec![PyRep::Integer(fix.d_ore_id)]),
                    PyRep::Integer(NPC_STATION_ID),
                    PyRep::Integer(CHAR_D_ID),
                    PyRep::Integer(fix.d_cont_id),
                    PyRep::Integer(4),
                ],
                _ => vec![
                    PyRep::List(vec![PyRep::Integer(own_ore)]),
                    PyRep::Integer(own_station),
                ],
            },
            _ => vec![
                PyRep::List(vec![PyRep::Integer(own_ore)]),
                PyRep::Integer(own_station),
            ],
        },
        // rpc-lint: allow covers both client invbroker and server-registered internal i2 handler
        "invbroker" | "i2" => match meth {
            "GetInventory" => vec![
                PyRep::Integer(if sit == Situation::C { 2 } else { 1 }),
                PyRep::Integer(match sit {
                    Situation::B => fix.b_cont_id,
                    Situation::C => CORP_A_OFFICE_ID,
                    _ => own_ship,
                }),
            ],
            "GetInventoryFromId" => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_cont_id,
                Situation::C => CORP_A_OFFICE_ID,
                _ => own_ship,
            })],
            "Add" | "MultiAdd" | "MultiMerge" => vec![
                PyRep::Integer(match sit {
                    Situation::B => fix.b_item_id,
                    Situation::C => CORP_A_OFFICE_ITEM_ID,
                    _ => own_item,
                }),
                PyRep::Integer(match sit {
                    Situation::B => fix.b_cont_id,
                    Situation::C => CORP_A_OFFICE_ID,
                    _ => own_cont,
                }),
            ],
            "TrashItems" | "SplitStack" | "SetLabel" | "AssembleCargoContainer" => {
                vec![
                    PyRep::List(vec![PyRep::Integer(match sit {
                        Situation::B => fix.b_item_id,
                        Situation::C => CORP_A_OFFICE_ITEM_ID,
                        _ => own_item,
                    })]),
                    PyRep::Integer(match sit {
                        Situation::B => fix.b_cont_id,
                        Situation::C => CORP_A_OFFICE_ID,
                        _ => own_cont,
                    }),
                ]
            }
            "DeliverToCorpHangar" | "DeliverToCorpMember" => vec![
                PyRep::Integer(own_office),
                PyRep::Integer(own_station),
                PyRep::List(vec![PyRep::Integer(own_corp_item)]),
                PyRep::Integer(116),
                PyRep::Integer(if sit == Situation::B { CORP_B_ID } else { own_corp }),
            ],
            _ => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_item_id,
                Situation::C => CORP_A_OFFICE_ITEM_ID,
                _ => own_item,
            })],
        },
        "beyonce" => match meth {
            "CmdWarpToStuff" | "CmdWarpToStuffAutopilot" => vec![
                PyRep::String("item".into()),
                PyRep::Integer(match sit {
                    // Situation E is in `other_sys_id`: pass cross-system Jita station (warping across systems is illegal).
                    Situation::E => NPC_STATION_ID,
                    _ => own_station,
                }),
                PyRep::Real(0.0),
            ],
            "CmdDock" | "CmdTurboDock" => vec![PyRep::Integer(own_station)],
            "CmdStargateJump" | "StargateJump" | "ActivateAccelerationGate" => {
                vec![PyRep::Integer(match sit {
                    Situation::E => fix.other_gate_id,
                    _ => fix.jita_gate_id,
                })]
            }
            "CmdOrbit" | "CmdFollowBall" | "CmdAlignTo" => vec![
                PyRep::Integer(match sit {
                    Situation::B => fix.b_space_ship_id,
                    // Situation E is in `other_sys_id`: pass cross-system Jita ship (align/follow/orbit across systems is illegal).
                    Situation::E => fix.b_space_ship_id,
                    Situation::F => fix.f_space_ship_id,
                    _ => fix.jita_gate_id,
                }),
                PyRep::Real(1_000.0),
            ],
            "CmdKeepAtRange" | "CmdAttackTarget" | "CmdMineRepeatedly" | "CmdAbandonLoot" => vec![
                PyRep::Integer(match sit {
                    Situation::B => fix.b_space_ship_id,
                    Situation::E => fix.e_space_ship_id,
                    Situation::F => fix.f_space_ship_id,
                    _ => fix.jita_gate_id,
                }),
                PyRep::Real(1_000.0),
            ],
            "CmdSetSpeedFraction" => vec![PyRep::Real(0.5)],
            "CmdGotoPoint" | "CmdGotoDirection" | "CmdSteerDirection" => {
                vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]
            }
            "CallComponentFromClient" => vec![
                PyRep::Integer(match sit {
                    Situation::B => fix.b_space_ship_id,
                    Situation::C => CORP_A_STRUCTURE_ID,
                    _ => own_space_ship,
                }),
                PyRep::String("itemTrader".into()),
                PyRep::String("ProcessTrade".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
            ],
            _ => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_space_ship_id,
                Situation::C => CORP_A_STRUCTURE_ID,
                Situation::E => fix.other_station_id,
                Situation::F => fix.f_space_ship_id,
                _ => fix.jita_gate_id,
            })],
        },
        "stationSvc" | "structureDocking" => match meth {
            "Undock" => vec![],
            "Dock" => vec![PyRep::Integer(if svc == "structureDocking" {
                own_structure
            } else {
                own_station
            })],
            _ => vec![PyRep::Integer(own_station)],
        },
        "ship" => match meth {
            "Board" => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_ship_id,
                Situation::C => CORP_A_STRUCTURE_ID,
                // Situations E/F test an undocked caller trying to board another ship in space
                // (500 km / ~299 km away), not re-boarding the active ship they already pilot.
                Situation::E | Situation::F => own_space_ship,
                _ => own_ship,
            })],
            "BoardStoredShip" | "AssembleShip" | "ConfigureShip" | "FitShips"
            | "StripFitting" => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_ship_id,
                Situation::C => CORP_A_STRUCTURE_ID,
                _ => own_ship,
            })],
            "Scoop" | "ScoopDrone" | "ScoopToDroneBay" | "ScoopToFleetHangar"
            | "ScoopToInfrastructureHold" | "ScoopToMobileDepotHold" | "ScoopToSMA"
            | "StoreVessel" => vec![PyRep::Integer(match sit {
                Situation::B | Situation::C => fix.b_space_ship_id,
                _ => own_space_ship,
            })],
            _ => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_ship_id,
                _ => own_ship,
            })],
        },
        "dogmaIM" => match meth {
            "Activate" | "Deactivate" | "Overload" | "StopOverload" | "OverloadRack"
            | "StopOverloadRack" | "SetModuleOnline" | "TakeModuleOffline" => vec![
                PyRep::Integer(own_module),
                PyRep::String("online".into()),
            ],
            "AddTarget" | "AddTargetOBO" | "RemoveTarget" => vec![PyRep::Integer(own_space_ship)],
            _ => vec![PyRep::Integer(match sit {
                Situation::B => fix.b_ship_id,
                _ => own_ship,
            })],
        },
        "account" => match meth {
            "GiveCashFromCorpAccount" => vec![
                PyRep::Integer(if sit == Situation::B {
                    CORP_B_ID
                } else {
                    own_corp
                }),
                PyRep::Integer(if sit == Situation::B { CHAR_A_ID } else { own_char }),
                PyRep::Real(1_000.0),
                PyRep::String("test".into()),
            ],
            "GiveCash" => vec![
                PyRep::Integer(if sit == Situation::B {
                    CHAR_B_ID
                } else {
                    CHAR_A_CEO_ID
                }),
                PyRep::Real(1_000.0),
                PyRep::String("test".into()),
            ],
            "GetWalletDivisionsInfo" => vec![PyRep::Integer(if sit == Situation::B {
                CORP_B_ID
            } else {
                own_corp
            })],
            "GetCashBalance" => vec![
                PyRep::Integer(match sit {
                    Situation::B => CHAR_B_ID,
                    Situation::C => CORP_A_ID,
                    _ => own_char,
                }),
                PyRep::Integer(1000),
            ],
            "GetTransactions" => vec![
                PyRep::Integer(1000),
                PyRep::None,
                PyRep::None,
                PyRep::Bool(sit == Situation::C),
            ],
            _ => vec![PyRep::Integer(own_char)],
        },
        "marketProxy" => match meth {
            "CancelCharOrder" | "ModifyCharOrder" => vec![
                PyRep::Integer(own_order),
                PyRep::Real(100.0),
            ],
            "PlaceBuyOrder" | "PlaceMultiSellOrder" | "BuyMultipleItems" => vec![
                PyRep::Integer(own_station),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(0),
            ],
            _ => vec![PyRep::Integer(34)],
        },
        "contractProxy" => match meth {
            "GetContract" | "AcceptContract" | "DeleteContract" | "CompleteContract"
            | "RejectContract" | "FailContract" | "PlaceBid" => {
                vec![PyRep::Integer(own_contract)]
            }
            _ => vec![PyRep::Integer(match sit {
                Situation::B => CHAR_B_ID,
                Situation::C => CORP_A_ID,
                _ => own_char,
            })],
        },
        "blueprintManager" => match meth {
            "GetBlueprintDataByOwner" => vec![PyRep::Integer(match sit {
                Situation::B => CHAR_B_ID,
                Situation::C => CORP_A_ID,
                _ => own_char,
            })],
            _ => vec![PyRep::Integer(own_blueprint)],
        },
        "industryManager" | "industryMonitor" => match meth {
            "GetJobsByOwner" => vec![PyRep::Integer(match sit {
                Situation::B => CHAR_B_ID,
                Situation::C => CORP_A_ID,
                _ => own_char,
            })],
            _ => vec![PyRep::Integer(own_job)],
        },
        "officeManager" => vec![PyRep::Integer(match sit {
            Situation::B => CORP_B_ID,
            Situation::C => match meth {
                "RentOffice" | "UnrentOffice" => NPC_STATION_ID,
                _ => CORP_A_ID,
            },
            _ => own_station,
        })],
        "corpmgr" | "corporationSvc" | "corpRegistry" | "corpStationMgr" | "voteManager"
        | "corpFittingMgr" | "corpMiningLedger" => vec![PyRep::Integer(if sit == Situation::B {
            CORP_B_ID
        } else {
            own_corp
        })],
        "corpProjects" => vec![PyRep::Integer(own_corp_project)],
        "structureDirectory" | "structureSettings" | "structureProfiles" | "structureGuests"
        | "structureHangarViewMgr" | "structureControl" | "structureDeployment" | "structure"
        | "structureAssetSafety" | "structureCompressionMgr" | "structureCynoBeaconMgr"
        | "structureJumpBridgeMgr" => vec![PyRep::Integer(own_structure)],
        "mailMgr" | "mailingListsMgr" => vec![PyRep::Integer(own_mail)],
        "bookmark" | "accessGroupBookmarkMgr" => vec![PyRep::Integer(own_bookmark)],
        "charFittingMgr" => match meth {
            "GetFittings" => vec![PyRep::Integer(match sit {
                Situation::B => CHAR_B_ID,
                _ => own_char,
            })],
            _ => vec![PyRep::Integer(own_fitting)],
        },
        "fleetMgr" | "fleetProxy" | "fleetObjectHandler" | "fleetSvc" => {
            vec![PyRep::Integer(own_fleet)]
        }
        "warStatisticMgr" if meth == "GetKillMail" => vec![PyRep::Integer(own_killmail)],
        "warRegistry" | "warStatisticMgr" | "warsInfoMgr" | "mutualWarInviteMgr"
        | "peaceTreatyMgr" => vec![PyRep::Integer(own_war)],
        "planetMgr" | "planetOrbitalRegistryBroker" => vec![PyRep::Integer(own_planet)],
        "posMgr" => vec![PyRep::Integer(own_pos)],
        "petitioner" => vec![PyRep::Integer(own_petition)],
        "itemLocking" => vec![PyRep::Integer(match sit {
            Situation::B => fix.b_ship_id,
            Situation::C => CORP_A_OFFICE_ID,
            _ => own_ship,
        })],
        "dungeonInstanceCacheMgr" if meth == "GetOreAnomaliesCountInRange" => {
            vec![PyRep::Integer(1)]
        }
        "charMgr" if meth == "SetHomeStation" => vec![PyRep::Integer(match sit {
            Situation::B => CHAR_B_ID,
            Situation::C => CORP_A_ID,
            // Situation E is undocked in `other_sys_id`: pass non-HQ, non-school station where pilot is not docked.
            Situation::E => fix.non_hq_station_id,
            Situation::F => fix.jita_gate_id,
            _ => own_char,
        })],
        "home_station" if meth == "set_home_station" => vec![PyRep::Integer(match sit {
            Situation::B => CHAR_B_ID,
            Situation::C => CORP_A_ID,
            // Situation E is undocked in `other_sys_id`: pass non-HQ, non-school station where pilot is not docked.
            Situation::E => fix.non_hq_station_id,
            Situation::F => fix.jita_gate_id,
            _ => own_char,
        })],
        "charUnboundMgr" => match meth {
            "SelectCharacterID" => vec![PyRep::Integer(match sit {
                Situation::A | Situation::B | Situation::C => CHAR_B_ID,
                _ => own_char,
            })],
            "GetCharacterInfo" | "GetCharactersToSelect" | "GetCharacterLockType"
            | "DeleteCharacter" | "PrepareCharacterForDelete"
            | "CancelCharacterDeletePrepare" => {
                vec![PyRep::Integer(match sit {
                    Situation::B => CHAR_B_ID,
                    _ => own_char,
                })]
            }
            _ => vec![PyRep::Integer(own_char)],
        },
        _ => vec![PyRep::Integer(match sit {
            Situation::B => CHAR_B_ID,
            Situation::C => CORP_A_ID,
            Situation::E => fix.other_station_id,
            Situation::F => fix.jita_gate_id,
            _ => own_char,
        })],
    };

    (bound_id, args)
}

// ============================================================================
// Single Authoritative Matrix & Regression Gate Test
// ============================================================================

fn text_mentions_any_id(raw: &str, ids: &[i64]) -> bool {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("char_b") || lower.contains("corp_b") || lower.contains("alliance_b") {
        return true;
    }
    raw.split(|c: char| !c.is_ascii_digit())
        .filter(|tok| tok.len() >= 6)
        .filter_map(|tok| tok.parse::<i64>().ok())
        .any(|n| ids.contains(&n))
}

fn json_mentions_any_id(val: &Value, ids: &[i64]) -> bool {
    match val {
        Value::Number(n) => {
            n.as_i64().is_some_and(|i| ids.contains(&i))
                || n.as_u64()
                    .and_then(|u| i64::try_from(u).ok())
                    .is_some_and(|i| ids.contains(&i))
        }
        Value::String(s) => text_mentions_any_id(s, ids),
        Value::Array(arr) => arr.iter().any(|v| json_mentions_any_id(v, ids)),
        Value::Object(obj) => obj
            .iter()
            .any(|(k, v)| text_mentions_any_id(k, ids) || json_mentions_any_id(v, ids)),
        Value::Null | Value::Bool(_) => false,
    }
}

fn is_entity_id_field(field: &str) -> bool {
    field == "id"
        || field.ends_with("_id")
        || field.ends_with("_ids")
        || field.ends_with("Id")
        || field.ends_with("Ids")
        || matches!(
            field,
            "owner"
                | "character"
                | "user"
                | "reporter"
                | "reported"
                | "target"
                | "issuer"
                | "assignee"
                | "members"
                | "occupants"
                | "participants"
                | "recipients"
                | "targets"
                | "invites"
                | "allow_list"
                | "deny_list"
        )
}

fn is_owner_id_field(field: &str) -> bool {
    matches!(
        field,
        "owner_char_id" | "owner_id" | "char_id" | "character_id" | "user_id"
    )
}

fn is_numeric_entity_key(k: &str) -> bool {
    !k.is_empty()
        && k.split(':')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn non_b_actor_ids() -> [i64; 18] {
    [
        CHAR_A_ID,
        USER_A_ID as i64,
        CORP_A_ID,
        CHAR_A_CEO_ID,
        USER_A_CEO_ID as i64,
        CORP_A_OFFICE_ID,
        CORP_A_OFFICE_ITEM_ID,
        CORP_A_STRUCTURE_ID,
        CHAR_C_ID,
        USER_C_ID as i64,
        CHAR_D_ID,
        USER_D_ID as i64,
        CORP_D_ID,
        CHAR_E_ID,
        USER_E_ID as i64,
        CORP_E_ID,
        CHAR_F_ID,
        CORP_F_ID,
    ]
}

fn object_directly_identifies_b(
    map: &serde_json::Map<String, Value>,
    b_ids: &[i64],
) -> bool {
    map.iter().any(|(k, v)| {
        is_entity_id_field(k)
            && (v.is_number() || v.is_string())
            && json_mentions_any_id(v, b_ids)
    })
}

fn object_belongs_strictly_to_non_b_owner(
    map: &serde_json::Map<String, Value>,
    b_ids: &[i64],
    non_b_ids: &[i64],
) -> bool {
    let owned_by_non_b = map.iter().any(|(k, v)| {
        is_owner_id_field(k)
            && (v.is_number() || v.is_string())
            && json_mentions_any_id(v, non_b_ids)
            && !json_mentions_any_id(v, b_ids)
    });
    if !owned_by_non_b {
        return false;
    }
    !map.iter()
        .any(|(k, v)| is_entity_id_field(k) && json_mentions_any_id(v, b_ids))
}

fn json_b_slice_inner(
    val: &Value,
    b_ids: &[i64],
    non_b_ids: &[i64],
    top_level: bool,
) -> Option<Value> {
    match val {
        Value::Object(map) => {
            if !top_level && object_directly_identifies_b(map, b_ids) {
                return Some(Value::Object(map.clone()));
            }
            if !top_level && object_belongs_strictly_to_non_b_owner(map, b_ids, non_b_ids) {
                return None;
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if text_mentions_any_id(k, b_ids) {
                    out.insert(k.clone(), v.clone());
                } else if is_numeric_entity_key(k) && text_mentions_any_id(k, non_b_ids) {
                    continue;
                } else if let Some(sub) = json_b_slice_inner(v, b_ids, non_b_ids, false) {
                    out.insert(k.clone(), sub);
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        Value::Array(items) => {
            let kept: Vec<Value> = items
                .iter()
                .filter(|v| json_mentions_any_id(v, b_ids))
                .cloned()
                .collect();
            (!kept.is_empty()).then_some(Value::Array(kept))
        }
        other => json_mentions_any_id(other, b_ids).then(|| other.clone()),
    }
}

fn note_or_json_b_slice(raw: Option<&String>, b_ids: &[i64]) -> Option<Value> {
    let raw = raw?;
    let non_b_ids = non_b_actor_ids();
    if let Ok(val) = serde_json::from_str::<Value>(raw) {
        return json_b_slice_inner(&val, b_ids, &non_b_ids, true);
    }
    text_mentions_any_id(raw, b_ids).then(|| Value::String(raw.clone()))
}

/// Returns `true` if any persisted or runtime state owned by Target B (`CHAR_B_ID`,
/// `USER_B_ID`, `CORP_B_ID`, `ALLIANCE_B_ID`, or B's seeded entities) — or global server
/// config/clock — diverged from `base`.
///
/// Used for Situation `"b"` RPCs explicitly allowlisted with `"caller_scoped_mutation:"` so
/// that mutations to the caller's own state (`CHAR_A_ID`) are permitted while Target B remains
/// strictly invariant.
fn target_b_state_mutated(
    fix: &MatrixFixture,
    base: &RuntimeBaseline,
    cur_db: Option<&WorldTablesSnapshot>,
    bound_dirty: bool,
    space_dirty: bool,
) -> bool {
    // Global server config and virtual clock must never be mutated by a caller-scoped RPC.
    if *fix
        .ctx
        .global_config_values
        .read()
        .expect("global_config_values")
        != base.global_config_values
        || fix.ctx.clock().now_unix_secs() != base.clock_unix
    {
        return true;
    }

    // Target B's warp/invuln runtime entries must remain invariant (acquire one lock at a time for lock-rank safety).
    let b_ship_ids = [fix.b_ship_id, fix.b_space_ship_id];
    let warp_dirty = fix
        .ctx
        .systems
        .active_system_handles()
        .into_iter()
        .any(|(sid, h)| {
            let cur = h.snapshot_warp_state_for_test();
            let base_ws = base.warp_states.get(&sid);
            b_ship_ids.iter().any(|id| {
                cur.active_warps.get(id) != base_ws.and_then(|b| b.active_warps.get(id))
                    || cur.return_warps.contains(id)
                        != base_ws.is_some_and(|b| b.return_warps.contains(id))
                    || cur.pending_warp_finished.contains(id)
                        != base_ws.is_some_and(|b| b.pending_warp_finished.contains(id))
            })
        });
    if warp_dirty {
        return true;
    }
    let inv_dirty = {
        let inv = fix.ctx.systems.snapshot_location_change_invuln();
        [CHAR_B_ID, fix.b_ship_id, fix.b_space_ship_id]
            .iter()
            .any(|id| inv.get(id) != base.location_change_invuln.get(id))
    };
    if inv_dirty {
        return true;
    }

    // Target B's bound object entries must remain invariant (no removal, overwrite, or new B binding).
    if bound_dirty {
        let cur_bounds = fix.ctx.bound_objects.read().expect("bound_objects");
        for (&id, b_entry) in &base.bound_keys {
            if b_entry.1 == USER_B_ID || b_entry.2 == CHAR_B_ID {
                let unchanged = cur_bounds.get(&id).is_some_and(|e| {
                    b_entry.0 == e.session_id
                        && b_entry.1 == e.user_id
                        && b_entry.2 == e.character_id
                        && b_entry.3 == e.service
                        && b_entry.4 == e.object_id
                });
                if !unchanged {
                    return true;
                }
            }
        }
        for (&id, e) in cur_bounds.iter() {
            if (e.user_id == USER_B_ID || e.character_id == CHAR_B_ID)
                && !base.bound_keys.contains_key(&id)
            {
                return true;
            }
        }
    }

fn target_b_entity_ids(fix: &MatrixFixture) -> Vec<i64> {
    [
        CHAR_B_ID,
        USER_B_ID as i64,
        CORP_B_ID,
        ALLIANCE_B_ID,
        fix.b_ship_id,
        fix.b_space_ship_id,
        fix.b_cont_id,
        fix.b_item_id,
        fix.b_blueprint_id,
        fix.b_pos_id,
        fix.b_fleet_id,
        fix.b_war_id,
        fix.b_planet_id,
        fix.b_order_id,
        fix.b_contract_id,
        fix.b_job_id,
        fix.b_fitting_id,
        fix.b_mail_id,
        fix.b_bookmark_id,
        fix.b_killmail_id,
        fix.b_corp_project_id,
        fix.b_petition_id,
        CORP_B_OFFICE_ID,
        CORP_B_OFFICE_ITEM_ID,
        CORP_B_STRUCTURE_ID,
    ]
    .into_iter()
    .filter(|&id| id > 0)
    .collect()
}

fn esi_target_b_mutated(fix: &MatrixFixture, base: &EsiRuntimeSig, cur: &EsiRuntimeSig, b_ids: &[i64]) -> bool {
    let b_del_before: BTreeSet<(i64, i64)> = base
        .deleted_mails
        .iter()
        .copied()
        .filter(|&(cid, mid)| cid == CHAR_B_ID || mid == fix.b_mail_id)
        .collect();
    let b_del_after: BTreeSet<(i64, i64)> = cur
        .deleted_mails
        .iter()
        .copied()
        .filter(|&(cid, mid)| cid == CHAR_B_ID || mid == fix.b_mail_id)
        .collect();
    if b_del_before != b_del_after {
        return true;
    }
    let meta_b = |m: &BTreeMap<(i64, i64), (bool, Vec<i64>)>| -> BTreeMap<(i64, i64), (bool, Vec<i64>)> {
        m.iter()
            .filter(|(&(cid, mid), _)| cid == CHAR_B_ID || mid == fix.b_mail_id)
            .map(|(&k, v)| (k, v.clone()))
            .collect()
    };
    if meta_b(&base.mail_meta) != meta_b(&cur.mail_meta)
        || base.mail_labels.get(&CHAR_B_ID) != cur.mail_labels.get(&CHAR_B_ID)
        || base.mail_lists.get(&CHAR_B_ID) != cur.mail_lists.get(&CHAR_B_ID)
        || base.char_contacts.get(&CHAR_B_ID) != cur.char_contacts.get(&CHAR_B_ID)
        || base.char_contact_labels.get(&CHAR_B_ID) != cur.char_contact_labels.get(&CHAR_B_ID)
        || base.corp_contacts.get(&CORP_B_ID) != cur.corp_contacts.get(&CORP_B_ID)
        || base.corp_contact_labels.get(&CORP_B_ID) != cur.corp_contact_labels.get(&CORP_B_ID)
        || base.alliance_contacts.get(&ALLIANCE_B_ID) != cur.alliance_contacts.get(&ALLIANCE_B_ID)
        || base.alliance_contact_labels.get(&ALLIANCE_B_ID)
            != cur.alliance_contact_labels.get(&ALLIANCE_B_ID)
        || base.contact_notifications.get(&CHAR_B_ID) != cur.contact_notifications.get(&CHAR_B_ID)
        || base.char_corp_history.get(&CHAR_B_ID) != cur.char_corp_history.get(&CHAR_B_ID)
        || base.char_titles.get(&CHAR_B_ID) != cur.char_titles.get(&CHAR_B_ID)
        || base.fleet_motd.get(&fix.b_fleet_id) != cur.fleet_motd.get(&fix.b_fleet_id)
        || base.war_allies.get(&fix.b_war_id) != cur.war_allies.get(&fix.b_war_id)
    {
        return true;
    }
    let cal_ev_b = |m: &BTreeMap<i64, (i64, u64, String, String, i64, i64, i64, String, String, Vec<i64>)>| {
        m.iter()
            .filter(|(_, ev)| {
                ev.6 == CHAR_B_ID
                    || ev.6 == CORP_B_ID
                    || ev.6 == ALLIANCE_B_ID
                    || ev.9.contains(&CHAR_B_ID)
            })
            .map(|(&k, v)| (k, v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let b_ev_before = cal_ev_b(&base.calendar_events);
    let b_ev_after = cal_ev_b(&cur.calendar_events);
    if b_ev_before != b_ev_after {
        return true;
    }
    let cal_resp_b = |m: &BTreeMap<(i64, i64), String>| {
        m.iter()
            .filter(|(&(eid, cid), _)| {
                cid == CHAR_B_ID || b_ev_before.contains_key(&eid) || b_ev_after.contains_key(&eid)
            })
            .map(|(&k, v)| (k, v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    if cal_resp_b(&base.calendar_responses) != cal_resp_b(&cur.calendar_responses) {
        return true;
    }
    let fleet_wings_b = |m: &BTreeMap<(i64, i64), String>| {
        m.iter()
            .filter(|(&(fid, _), _)| fid == fix.b_fleet_id)
            .map(|(&k, v)| (k, v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let fleet_squads_b = |m: &BTreeMap<(i64, i64), (i64, String)>| {
        m.iter()
            .filter(|(&(fid, _), _)| fid == fix.b_fleet_id)
            .map(|(&k, v)| (k, v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let fleet_members_b = |m: &BTreeMap<(i64, i64), (String, i64, i64)>| {
        m.iter()
            .filter(|(&(fid, cid), _)| fid == fix.b_fleet_id || cid == CHAR_B_ID)
            .map(|(&k, v)| (k, v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    if fleet_wings_b(&base.fleet_wing_names) != fleet_wings_b(&cur.fleet_wing_names)
        || fleet_squads_b(&base.fleet_squads) != fleet_squads_b(&cur.fleet_squads)
        || fleet_members_b(&base.fleet_member_meta) != fleet_members_b(&cur.fleet_member_meta)
    {
        return true;
    }
    b_ids
        .iter()
        .any(|id| base.custom_item_names.get(id) != cur.custom_item_names.get(id))
}

    // Target B's in-space entities (ship, Upwell structure, POS tower) and target locks must remain invariant.
    let b_entity_ids = target_b_entity_ids(fix);
    let b_balls = [fix.b_space_ship_id, CORP_B_STRUCTURE_ID, fix.b_pos_id];
    if space_dirty {
        for bp_base in &base.ballparks {
            let sys = fix.ctx.systems.get_or_spawn(bp_base.sys_id);
            let bp = sys.ballpark.read().expect("ballpark");
            for &bid in &b_balls {
                if bp.balls.get(&bid) != bp_base.balls.get(&bid) {
                    return true;
                }
                let base_cs = bp_base.combat_states.get(&bid).map(combat_sig);
                let cur_cs = bp.combat_states.get(&bid).map(combat_sig);
                if base_cs != cur_cs {
                    return true;
                }
            }
            if target_locks_b_sigs(&bp.target_locks, &b_balls)
                != target_locks_b_sigs(&bp_base.target_locks, &b_balls)
            {
                return true;
            }
        }
    }

    // Target B runtime maps (crimewatch, esi_state, social trashed mails, fleet extras, combat/tactical).
    if fix.ctx.crimewatch.state_of(CHAR_B_ID) != base.crimewatch_b_state {
        return true;
    }
    let cw_b_dirty = {
        let cw = fix.ctx.crimewatch.read().expect("crimewatch");
        let b_aggr_before: BTreeMap<(i64, i64), &PersistedAggressionRecord> = base
            .crimewatch_aggressions
            .iter()
            .filter(|(&(a, v), _)| a == CHAR_B_ID || v == CHAR_B_ID)
            .map(|(&k, r)| (k, r))
            .collect();
        let b_aggr_after: BTreeMap<(i64, i64), &PersistedAggressionRecord> = cw
            .aggression_mgr
            .records
            .iter()
            .filter(|(&(a, v), _)| a == CHAR_B_ID || v == CHAR_B_ID)
            .map(|(&k, r)| (k, r))
            .collect();
        let b_despawns_before: Vec<_> = base
            .crimewatch_pending_despawns
            .iter()
            .filter(|&&(_, cid, _)| cid == CHAR_B_ID)
            .collect();
        let b_despawns_after: Vec<_> = cw
            .pending_concord_despawns
            .iter()
            .filter(|&&(_, cid, _)| cid == CHAR_B_ID)
            .collect();
        b_aggr_before != b_aggr_after || b_despawns_before != b_despawns_after
    };
    if cw_b_dirty {
        return true;
    }
    let cur_esi = capture_esi_sig(&fix.ctx.esi_state);
    if esi_target_b_mutated(fix, &base.esi.sig, &cur_esi, &b_entity_ids) {
        return true;
    }
    let cur_trashed_b: BTreeSet<(i64, i64)> =
        eve_server::services::fleet_social::get_trashed_mails(&fix.ctx)
            .into_iter()
            .filter(|&(cid, _)| cid == CHAR_B_ID)
            .collect();
    let base_trashed_b: BTreeSet<(i64, i64)> = base
        .trashed_mails
        .iter()
        .copied()
        .filter(|&(cid, _)| cid == CHAR_B_ID)
        .collect();
    let cur_tbrt = capture_target_b_runtime(fix);
    if cur_trashed_b != base_trashed_b
        || fix.ctx.has_fleet_extras_for(fix.b_fleet_id) != base.b_fleet_has_extras
        || cur_tbrt != base.target_b_runtime
    {
        return true;
    }

    let Some(cur_tables) = cur_db else {
        return false;
    };
    let b = &base.db.snapshot;
    let c = &cur_tables.snapshot;
    // Exhaustively destructure every field of `WorldStateSnapshot` (matching `snapshot_ext.rs`)
    // so new snapshot fields cannot be silently omitted from Target B invariance checks.
    let WorldStateSnapshot {
        next_entity_id: _,
        wal_seq: _,
        markets_seeded: _,
        service_state_blobs: ref sb_service_state_blobs,
        sessions: ref sb_sessions,
        accounts: _,
        characters: _,
        skills: _,
        skill_queues: _,
        jump_clones: _,
        active_implants: _,
        items: ref sb_items,
        transient_items: ref sb_transient_items,
        transient_item_attributes: ref sb_transient_item_attributes,
        saved_fittings: _,
        market_orders: ref sb_market_orders,
        wallet_journal: ref sb_wallet_journal,
        contracts: _,
        industry_jobs: _,
        planetary_colonies: _,
        active_missions: ref sb_active_missions,
        mission_tally: ref sb_mission_tally,
        standing_transactions: _,
        npe_progress: _,
        bookmarks: ref sb_bookmarks,
        mails: ref sb_mails,
        fleets: ref sb_fleets,
        corporations: _,
        quasar_state_blobs: ref sb_quasar_state_blobs,
        alliances: _,
        wars: _,
        corp_projects: ref sb_corp_projects,
        killmails: ref sb_killmails,
        kill_rights: ref sb_kill_rights,
        structures: ref sb_structures,
        system_states: ref sb_system_states,
        committed_idempotency_keys: ref sb_committed_idempotency_keys,
        licensed_skins: _,
        applied_ship_skins: _,
        chat_channels: ref sb_chat_channels,
        cosmic_signatures: _,
        read_mails: _,
        deleted_mails: _,
        mail_labels: _,
        mail_assigned_labels: _,
        bookmark_folders: ref sb_bookmark_folders,
        wh_polarizations: ref sb_wh_polarizations,
        mining_ledger: ref sb_mining_ledger,
        module_online_states: _,
        module_active_states: _,
        module_active_charges: _,
        ship_fit_inputs: _,
        weapon_banks: _,
        asteroids: _,
        moon_extractions: ref sb_moon_extractions,
        auto_moon_miners: ref sb_auto_moon_miners,
        observer_mining_ledger: ref sb_observer_mining_ledger,
        owner_notes: _,
        paused_skill_queues: _,
        cosmic_anomalies: _,
        sov_prospecting_levels: _,
        last_mission_decline_times: ref sb_last_mission_decline_times,
        character_last_jump_unix: ref sb_character_last_jump_unix,
        character_cspa_charges: ref sb_character_cspa_charges,
        mail_timestamps: ref sb_mail_timestamps,
        character_login_stats: ref sb_character_login_stats,
        item_custom_names: ref sb_item_custom_names,
        item_positions: _,
        structure_vulnerability_windows: _,
        skill_history: _,
        character_doll_info: _,
        system_jump_events: _,
        system_kill_events: _,
        incursions: _,
        sov_campaign_scores: _,
        fw_character_stats: _,
        fw_corp_enlisted_on: ref sb_fw_corp_enlisted_on,
        fw_system_occupations: _,
        fw_wars: ref sb_fw_wars,
        war_lifecycle_meta: _,
        killmail_details: _,
        station_office_rental_costs: _,
        alliance_lifecycle_meta: _,
        corp_hangar_divisions: _,
        structure_esi_timer_meta: _,
        corp_alliance_history: _,
        corp_container_logs: ref sb_corp_container_logs,
        corp_medals: _,
        corp_issued_medals: _,
        corp_member_limits: _,
        corp_titles: _,
        corp_member_titles: _,
        corp_role_assignments: _,
        corp_role_history: _,
        corp_shareholders: _,
        starbase_settings: _,
        corp_project_contributors: ref sb_corp_project_contributors,
        char_research_agents: ref sb_char_research_agents,
        char_medals: _,
        char_scoped_roles: _,
        loyalty_store_offers: _,
        wallet_journal_timestamps: ref sb_wallet_journal_timestamps,
        char_wallet_transactions: _,
        corp_wallet_journals: _,
        corp_wallet_transactions: _,
        market_order_meta: ref sb_market_order_meta,
        market_history: _,
        planetary_colony_layouts: _,
        corp_customs_offices: _,
        wallet_journal_meta: ref sb_wallet_journal_meta,
        market_transactions: ref sb_market_transactions,
        account_plex_vault: _,
        corp_vote_cases: ref sb_corp_vote_cases,
        corp_vote_ballots: ref sb_corp_vote_ballots,
        orbital_provision_types: ref sb_orbital_provision_types,
        corp_lp_balances: _,
        char_lp_logs: _,
        next_alliance_id: _,
        next_corporation_id: _,
        next_notification_id: _,
        next_report_id: _,
        trashed_mails: _,
        mailing_lists: ref sb_mailing_lists,
        calendar_events: ref sb_calendar_events,
        char_contacts: ref sb_char_contacts,
        char_contact_notifications: ref sb_char_contact_notifications,
        char_contact_labels: ref sb_char_contact_labels,
        corp_contacts: ref sb_corp_contacts,
        corp_contact_labels: ref sb_corp_contact_labels,
        alliance_contacts: ref sb_alliance_contacts,
        alliance_contact_labels: ref sb_alliance_contact_labels,
        char_notes: ref sb_char_notes,
        bookmark_subfolders: ref sb_bookmark_subfolders,
        bookmark_subfolder_parents: ref sb_bookmark_subfolder_parents,
        bookmark_expiries: ref sb_bookmark_expiries,
        bookmark_folder_acls: ref sb_bookmark_folder_acls,
        access_groups: ref sb_access_groups,
        bookmark_folder_known_by: ref sb_bookmark_folder_known_by,
        chat_channel_acl: ref sb_chat_channel_acl,
        corp_governance: _,
        employment_history: _,
        alliance_war_state: ref sb_alliance_war_state,
        war_state: ref sb_war_state,
        fw_runtime: ref sb_fw_runtime,
        sov_runtime: ref sb_sov_runtime,
        insurgency_runtime: ref sb_insurgency_runtime,
        crimewatch_states: _,
        crimewatch_aggressions: ref sb_crimewatch_aggressions,
        combat_crimewatch_state: ref sb_combat_crimewatch_state,
        zarzakh_gate_locks: ref sb_zarzakh_gate_locks,
        structure_docking_acl_denied: _,
        sp6_lifecycle: ref sb_sp6_lifecycle,
        pve_special_sites: ref sb_pve_special_sites,
        crab_beacons: ref sb_crab_beacons,
        abyssal_state: ref sb_abyssal_state,
        dungeon_runtime: ref sb_dungeon_runtime,
        dungeon_editor: ref sb_dungeon_editor,
        fob_observatory_state: ref sb_fob_observatory_state,
        mining_and_ess_state: ref sb_mining_and_ess_state,
        pvp_filament_state: ref sb_pvp_filament_state,
        tourney_state: ref sb_tourney_state,
        deployable_state: ref sb_deployable_state,
        superweapon_runtime: ref sb_superweapon_runtime,
        insurgency_complex_ops: ref sb_insurgency_complex_ops,
        node_graph_state: ref sb_node_graph_state,
        seasons_and_campaigns: ref sb_seasons_and_campaigns,
        quasar_commerce: ref sb_quasar_commerce,
        fleet_and_social_ops: ref sb_fleet_and_social_ops,
        cluster_and_runtime: ref sb_cluster_and_runtime,
        entity_kv: _,
        insurance_contracts: _,
    } = *b;
    let WorldStateSnapshot {
        next_entity_id: _,
        wal_seq: _,
        markets_seeded: _,
        service_state_blobs: ref sc_service_state_blobs,
        sessions: ref sc_sessions,
        accounts: _,
        characters: _,
        skills: _,
        skill_queues: _,
        jump_clones: _,
        active_implants: _,
        items: ref sc_items,
        transient_items: ref sc_transient_items,
        transient_item_attributes: ref sc_transient_item_attributes,
        saved_fittings: _,
        market_orders: ref sc_market_orders,
        wallet_journal: ref sc_wallet_journal,
        contracts: _,
        industry_jobs: _,
        planetary_colonies: _,
        active_missions: ref sc_active_missions,
        mission_tally: ref sc_mission_tally,
        standing_transactions: _,
        npe_progress: _,
        bookmarks: ref sc_bookmarks,
        mails: ref sc_mails,
        fleets: ref sc_fleets,
        corporations: _,
        quasar_state_blobs: ref sc_quasar_state_blobs,
        alliances: _,
        wars: _,
        corp_projects: ref sc_corp_projects,
        killmails: ref sc_killmails,
        kill_rights: ref sc_kill_rights,
        structures: ref sc_structures,
        system_states: ref sc_system_states,
        committed_idempotency_keys: ref sc_committed_idempotency_keys,
        licensed_skins: _,
        applied_ship_skins: _,
        chat_channels: ref sc_chat_channels,
        cosmic_signatures: _,
        read_mails: _,
        deleted_mails: _,
        mail_labels: _,
        mail_assigned_labels: _,
        bookmark_folders: ref sc_bookmark_folders,
        wh_polarizations: ref sc_wh_polarizations,
        mining_ledger: ref sc_mining_ledger,
        module_online_states: _,
        module_active_states: _,
        module_active_charges: _,
        ship_fit_inputs: _,
        weapon_banks: _,
        asteroids: _,
        moon_extractions: ref sc_moon_extractions,
        auto_moon_miners: ref sc_auto_moon_miners,
        observer_mining_ledger: ref sc_observer_mining_ledger,
        owner_notes: _,
        paused_skill_queues: _,
        cosmic_anomalies: _,
        sov_prospecting_levels: _,
        last_mission_decline_times: ref sc_last_mission_decline_times,
        character_last_jump_unix: ref sc_character_last_jump_unix,
        character_cspa_charges: ref sc_character_cspa_charges,
        mail_timestamps: ref sc_mail_timestamps,
        character_login_stats: ref sc_character_login_stats,
        item_custom_names: ref sc_item_custom_names,
        item_positions: _,
        structure_vulnerability_windows: _,
        skill_history: _,
        character_doll_info: _,
        system_jump_events: _,
        system_kill_events: _,
        incursions: _,
        sov_campaign_scores: _,
        fw_character_stats: _,
        fw_corp_enlisted_on: ref sc_fw_corp_enlisted_on,
        fw_system_occupations: _,
        fw_wars: ref sc_fw_wars,
        war_lifecycle_meta: _,
        killmail_details: _,
        station_office_rental_costs: _,
        alliance_lifecycle_meta: _,
        corp_hangar_divisions: _,
        structure_esi_timer_meta: _,
        corp_alliance_history: _,
        corp_container_logs: ref sc_corp_container_logs,
        corp_medals: _,
        corp_issued_medals: _,
        corp_member_limits: _,
        corp_titles: _,
        corp_member_titles: _,
        corp_role_assignments: _,
        corp_role_history: _,
        corp_shareholders: _,
        starbase_settings: _,
        corp_project_contributors: ref sc_corp_project_contributors,
        char_research_agents: ref sc_char_research_agents,
        char_medals: _,
        char_scoped_roles: _,
        loyalty_store_offers: _,
        wallet_journal_timestamps: ref sc_wallet_journal_timestamps,
        char_wallet_transactions: _,
        corp_wallet_journals: _,
        corp_wallet_transactions: _,
        market_order_meta: ref sc_market_order_meta,
        market_history: _,
        planetary_colony_layouts: _,
        corp_customs_offices: _,
        wallet_journal_meta: ref sc_wallet_journal_meta,
        market_transactions: ref sc_market_transactions,
        account_plex_vault: _,
        corp_vote_cases: ref sc_corp_vote_cases,
        corp_vote_ballots: ref sc_corp_vote_ballots,
        orbital_provision_types: ref sc_orbital_provision_types,
        corp_lp_balances: _,
        char_lp_logs: _,
        next_alliance_id: _,
        next_corporation_id: _,
        next_notification_id: _,
        next_report_id: _,
        trashed_mails: _,
        mailing_lists: ref sc_mailing_lists,
        calendar_events: ref sc_calendar_events,
        char_contacts: ref sc_char_contacts,
        char_contact_notifications: ref sc_char_contact_notifications,
        char_contact_labels: ref sc_char_contact_labels,
        corp_contacts: ref sc_corp_contacts,
        corp_contact_labels: ref sc_corp_contact_labels,
        alliance_contacts: ref sc_alliance_contacts,
        alliance_contact_labels: ref sc_alliance_contact_labels,
        char_notes: ref sc_char_notes,
        bookmark_subfolders: ref sc_bookmark_subfolders,
        bookmark_subfolder_parents: ref sc_bookmark_subfolder_parents,
        bookmark_expiries: ref sc_bookmark_expiries,
        bookmark_folder_acls: ref sc_bookmark_folder_acls,
        access_groups: ref sc_access_groups,
        bookmark_folder_known_by: ref sc_bookmark_folder_known_by,
        chat_channel_acl: ref sc_chat_channel_acl,
        corp_governance: _,
        employment_history: _,
        alliance_war_state: ref sc_alliance_war_state,
        war_state: ref sc_war_state,
        fw_runtime: ref sc_fw_runtime,
        sov_runtime: ref sc_sov_runtime,
        insurgency_runtime: ref sc_insurgency_runtime,
        crimewatch_states: _,
        crimewatch_aggressions: ref sc_crimewatch_aggressions,
        combat_crimewatch_state: ref sc_combat_crimewatch_state,
        zarzakh_gate_locks: ref sc_zarzakh_gate_locks,
        structure_docking_acl_denied: _,
        sp6_lifecycle: ref sc_sp6_lifecycle,
        pve_special_sites: ref sc_pve_special_sites,
        crab_beacons: ref sc_crab_beacons,
        abyssal_state: ref sc_abyssal_state,
        dungeon_runtime: ref sc_dungeon_runtime,
        dungeon_editor: ref sc_dungeon_editor,
        fob_observatory_state: ref sc_fob_observatory_state,
        mining_and_ess_state: ref sc_mining_and_ess_state,
        pvp_filament_state: ref sc_pvp_filament_state,
        tourney_state: ref sc_tourney_state,
        deployable_state: ref sc_deployable_state,
        superweapon_runtime: ref sc_superweapon_runtime,
        insurgency_complex_ops: ref sc_insurgency_complex_ops,
        node_graph_state: ref sc_node_graph_state,
        seasons_and_campaigns: ref sc_seasons_and_campaigns,
        quasar_commerce: ref sc_quasar_commerce,
        fleet_and_social_ops: ref sc_fleet_and_social_ops,
        cluster_and_runtime: ref sc_cluster_and_runtime,
        entity_kv: _,
        insurance_contracts: _,
    } = *c;

    // 1. Direct Character B / Account B / Corp B / Alliance B keyed tables.
    if b.accounts.get("neg_b") != c.accounts.get("neg_b")
        || b.account_plex_vault.get(&USER_B_ID) != c.account_plex_vault.get(&USER_B_ID)
        || b.characters.get(&CHAR_B_ID) != c.characters.get(&CHAR_B_ID)
        || b.skills.get(&CHAR_B_ID) != c.skills.get(&CHAR_B_ID)
        || b.skill_queues.get(&CHAR_B_ID) != c.skill_queues.get(&CHAR_B_ID)
        || b.paused_skill_queues.contains(&CHAR_B_ID) != c.paused_skill_queues.contains(&CHAR_B_ID)
        || b.skill_history.get(&CHAR_B_ID) != c.skill_history.get(&CHAR_B_ID)
        || b.jump_clones.get(&CHAR_B_ID) != c.jump_clones.get(&CHAR_B_ID)
        || b.active_implants.get(&CHAR_B_ID) != c.active_implants.get(&CHAR_B_ID)
        || b.npe_progress.get(&CHAR_B_ID) != c.npe_progress.get(&CHAR_B_ID)
        || b.standing_transactions.get(&CHAR_B_ID) != c.standing_transactions.get(&CHAR_B_ID)
        || b.char_contacts.get(&CHAR_B_ID) != c.char_contacts.get(&CHAR_B_ID)
        || b.char_contact_labels.get(&CHAR_B_ID) != c.char_contact_labels.get(&CHAR_B_ID)
        || b.char_contact_notifications.get(&CHAR_B_ID)
            != c.char_contact_notifications.get(&CHAR_B_ID)
        || b.character_cspa_charges.get(&CHAR_B_ID) != c.character_cspa_charges.get(&CHAR_B_ID)
        || b.char_wallet_transactions.get(&CHAR_B_ID)
            != c.char_wallet_transactions.get(&CHAR_B_ID)
        || b.char_medals.get(&CHAR_B_ID) != c.char_medals.get(&CHAR_B_ID)
        || b.char_scoped_roles.get(&CHAR_B_ID) != c.char_scoped_roles.get(&CHAR_B_ID)
        || b.character_doll_info.get(&CHAR_B_ID) != c.character_doll_info.get(&CHAR_B_ID)
        || b.character_login_stats.get(&CHAR_B_ID) != c.character_login_stats.get(&CHAR_B_ID)
        || b.char_lp_logs.as_cow_map().get(&CHAR_B_ID)
            != c.char_lp_logs.as_cow_map().get(&CHAR_B_ID)
        || b.licensed_skins.get(&CHAR_B_ID) != c.licensed_skins.get(&CHAR_B_ID)
        || b.crimewatch_states.get(&CHAR_B_ID) != c.crimewatch_states.get(&CHAR_B_ID)
        || b.mail_labels.get(&CHAR_B_ID) != c.mail_labels.get(&CHAR_B_ID)
        || b.employment_history.get(&CHAR_B_ID) != c.employment_history.get(&CHAR_B_ID)
        || b.fw_character_stats.get(&CHAR_B_ID) != c.fw_character_stats.get(&CHAR_B_ID)
        || b.last_mission_decline_times.get(&CHAR_B_ID)
            != c.last_mission_decline_times.get(&CHAR_B_ID)
        || b.character_last_jump_unix.get(&CHAR_B_ID) != c.character_last_jump_unix.get(&CHAR_B_ID)
        || b.corporations.get(&CORP_B_ID) != c.corporations.get(&CORP_B_ID)
        || b.corp_governance.get(&CORP_B_ID) != c.corp_governance.get(&CORP_B_ID)
        || b.corp_hangar_divisions.get(&CORP_B_ID) != c.corp_hangar_divisions.get(&CORP_B_ID)
        || b.corp_contacts.get(&CORP_B_ID) != c.corp_contacts.get(&CORP_B_ID)
        || b.corp_contact_labels.get(&CORP_B_ID) != c.corp_contact_labels.get(&CORP_B_ID)
        || b.corp_medals.get(&CORP_B_ID) != c.corp_medals.get(&CORP_B_ID)
        || b.corp_issued_medals.get(&CORP_B_ID) != c.corp_issued_medals.get(&CORP_B_ID)
        || b.corp_member_limits.get(&CORP_B_ID) != c.corp_member_limits.get(&CORP_B_ID)
        || b.corp_titles.get(&CORP_B_ID) != c.corp_titles.get(&CORP_B_ID)
        || b.corp_role_history.get(&CORP_B_ID) != c.corp_role_history.get(&CORP_B_ID)
        || b.corp_shareholders.get(&CORP_B_ID) != c.corp_shareholders.get(&CORP_B_ID)
        || b.corp_alliance_history.get(&CORP_B_ID) != c.corp_alliance_history.get(&CORP_B_ID)
        || b.corp_customs_offices.get(&CORP_B_ID) != c.corp_customs_offices.get(&CORP_B_ID)
        || b.structure_vulnerability_windows.get(&CORP_B_STRUCTURE_ID)
            != c.structure_vulnerability_windows.get(&CORP_B_STRUCTURE_ID)
        || b.structure_esi_timer_meta.get(&CORP_B_STRUCTURE_ID)
            != c.structure_esi_timer_meta.get(&CORP_B_STRUCTURE_ID)
        || b.structure_docking_acl_denied.get(&CORP_B_STRUCTURE_ID)
            != c.structure_docking_acl_denied.get(&CORP_B_STRUCTURE_ID)
        || b.starbase_settings.get(&fix.b_pos_id) != c.starbase_settings.get(&fix.b_pos_id)
        || b.alliances.get(&ALLIANCE_B_ID) != c.alliances.get(&ALLIANCE_B_ID)
        || b.alliance_contacts.get(&ALLIANCE_B_ID) != c.alliance_contacts.get(&ALLIANCE_B_ID)
        || b.alliance_contact_labels.get(&ALLIANCE_B_ID)
            != c.alliance_contact_labels.get(&ALLIANCE_B_ID)
        || b.alliance_lifecycle_meta.get(&ALLIANCE_B_ID)
            != c.alliance_lifecycle_meta.get(&ALLIANCE_B_ID)
        || b.fleets.get(&fix.b_fleet_id) != c.fleets.get(&fix.b_fleet_id)
        || b.wars.get(&fix.b_war_id) != c.wars.get(&fix.b_war_id)
        || b.war_lifecycle_meta.get(&fix.b_war_id) != c.war_lifecycle_meta.get(&fix.b_war_id)
        || b.killmails.get(&fix.b_killmail_id) != c.killmails.get(&fix.b_killmail_id)
        || b.killmail_details.get(&fix.b_killmail_id) != c.killmail_details.get(&fix.b_killmail_id)
    {
        return true;
    }

    // 2. Target B items, ships, containers, hangars, modules, transient items, and structures.
    // Build Target B's full dynamic structure, item, transient-item, and jump-clone ID sets
    // up front so item_positions, applied_ship_skins, module_* maps, and side-stores cover
    // newly spawned / fitted / nested Target B entities as well as seeded fixtures.
    let mut b_struct_ids: BTreeSet<i64> =
        [CORP_B_STRUCTURE_ID, fix.b_pos_id].into_iter().collect();
    for (&sid, st) in sb_structures.iter().chain(sc_structures.iter()) {
        if st.owner_corp_id == CORP_B_ID {
            b_struct_ids.insert(sid);
        }
    }
    let mut b_item_ids: BTreeSet<i64> = b_entity_ids.iter().copied().collect();
    b_item_ids.extend(b_struct_ids.iter().copied());
    for cl in [b.jump_clones.get(&CHAR_B_ID), c.jump_clones.get(&CHAR_B_ID)]
        .into_iter()
        .flatten()
        .flatten()
    {
        b_item_ids.insert(cl.jump_clone_id);
    }
    loop {
        let mut added = false;
        for (&id, it) in sb_items
            .iter()
            .chain(sc_items.iter())
            .chain(sb_transient_items.iter())
            .chain(sc_transient_items.iter())
        {
            if it.owner_id == CHAR_B_ID
                || it.owner_id == CORP_B_ID
                || b_item_ids.contains(&id)
                || b_item_ids.contains(&it.location_id)
            {
                added |= b_item_ids.insert(id);
            }
        }
        if !added {
            break;
        }
    }
    let b_all_ids: Vec<i64> = b_item_ids
        .iter()
        .copied()
        .filter(|&id| id >= 100_000)
        .collect();

    if filtered_entries_changed(sb_items.iter(), sc_items.iter(), |&id, it| {
        b_item_ids.contains(&id)
            || it.owner_id == CHAR_B_ID
            || it.owner_id == CORP_B_ID
            || b_item_ids.contains(&it.location_id)
    }) || filtered_entries_changed(
        sb_transient_items.iter(),
        sc_transient_items.iter(),
        |&id, it| {
            b_item_ids.contains(&id)
                || it.owner_id == CHAR_B_ID
                || it.owner_id == CORP_B_ID
                || b_item_ids.contains(&it.location_id)
        },
    ) || filtered_entries_changed(
        sb_transient_item_attributes.iter(),
        sc_transient_item_attributes.iter(),
        |id, _| b_item_ids.contains(id),
    ) || filtered_entries_changed(
        sb_orbital_provision_types.iter(),
        sc_orbital_provision_types.iter(),
        |id, _| b_item_ids.contains(id),
    ) {
        return true;
    }
    for &iid in &b_item_ids {
        if b.item_custom_names.get(&iid) != c.item_custom_names.get(&iid)
            || b.item_positions.get(&iid) != c.item_positions.get(&iid)
            || b.applied_ship_skins.get(&iid) != c.applied_ship_skins.get(&iid)
            || b.module_online_states.get(&iid) != c.module_online_states.get(&iid)
            || b.module_active_states.contains(&iid) != c.module_active_states.contains(&iid)
            || b.module_active_charges.get(&iid) != c.module_active_charges.get(&iid)
            || b.ship_fit_inputs.get(&iid) != c.ship_fit_inputs.get(&iid)
            || b.weapon_banks.get(&iid) != c.weapon_banks.get(&iid)
        {
            return true;
        }
    }
    let b_structs_before: BTreeMap<i64, &UpwellStructureRecord> = b
        .structures
        .iter()
        .filter(|(&id, st)| id == CORP_B_STRUCTURE_ID || st.owner_corp_id == CORP_B_ID)
        .map(|(&id, st)| (id, st))
        .collect();
    let b_structs_after: BTreeMap<i64, &UpwellStructureRecord> = c
        .structures
        .iter()
        .filter(|(&id, st)| id == CORP_B_STRUCTURE_ID || st.owner_corp_id == CORP_B_ID)
        .map(|(&id, st)| (id, st))
        .collect();
    if b_structs_before != b_structs_after {
        return true;
    }

    // 3. Target B secondary records (fittings, orders, contracts, jobs, bookmarks, notes, mails, journal, PI, votes, KV).
    fn filtered_entries_changed<'a, K, V, I1, I2, P>(iter_b: I1, iter_c: I2, mut pred: P) -> bool
    where
        K: Ord + 'a,
        V: PartialEq + 'a,
        I1: IntoIterator<Item = (&'a K, &'a V)>,
        I2: IntoIterator<Item = (&'a K, &'a V)>,
        P: FnMut(&K, &V) -> bool,
    {
        let before: BTreeMap<&'a K, &'a V> = iter_b
            .into_iter()
            .filter(|&(k, v)| pred(k, v))
            .collect();
        let after: BTreeMap<&'a K, &'a V> = iter_c
            .into_iter()
            .filter(|&(k, v)| pred(k, v))
            .collect();
        before != after
    }

    // Use full `b_item_ids` / `b_all_ids` (including dynamic items, ships, containers, and fixtures).
    let b_neg_owner_note_ids: BTreeSet<i64> = [
        -10_000_099_i64 - CHAR_B_ID,
        -10_000_018_i64 - CHAR_B_ID,
        -8_950_000_000_000_i64 - CHAR_B_ID,
        -(5_100_000_000_000_i64.saturating_add(CHAR_B_ID.abs())),
        -(5_400_000_000_000_i64.saturating_add(CHAR_B_ID.abs())),
        -(5_400_000_000_000_i64.saturating_add(CORP_B_ID.abs())),
    ]
    .into_iter()
    .chain(
        b_struct_ids
            .iter()
            .map(|&sid| -(1_000_000_000_000_i64.saturating_add(sid.abs()))),
    )
    .chain(
        b_all_ids
            .iter()
            .map(|&id| -(4_000_000_000_000_i64.saturating_add(id.abs()))),
    )
    .collect();
    let owner_notes_b_slice = |notes: &WorldStateSnapshot| {
        let mut out: BTreeMap<i64, (i64, i64, String, Option<Value>, Option<String>)> =
            BTreeMap::new();
        for (&nid, n) in notes.owner_notes.iter() {
            let direct = n.character_id == CHAR_B_ID
                || n.character_id == CORP_B_ID
                || b_item_ids.contains(&n.item_id)
                || b_item_ids.contains(&nid)
                || b_neg_owner_note_ids.contains(&nid);
            if direct {
                out.insert(
                    nid,
                    (
                        n.character_id,
                        n.item_id,
                        n.label.clone(),
                        None,
                        Some(n.note.clone()),
                    ),
                );
            } else if nid < 0 || n.character_id < 0 {
                let label_b = text_mentions_any_id(&n.label, &b_all_ids);
                let sliced = note_or_json_b_slice(Some(&n.note), &b_all_ids);
                if label_b || sliced.is_some() {
                    out.insert(
                        nid,
                        (
                            n.character_id,
                            n.item_id,
                            n.label.clone(),
                            sliced,
                            label_b.then(|| n.note.clone()),
                        ),
                    );
                }
            }
        }
        out
    };
    if filtered_entries_changed(b.saved_fittings.iter(), c.saved_fittings.iter(), |&id, f| {
        id == fix.b_fitting_id || f.owner_id == CHAR_B_ID || f.owner_id == CORP_B_ID
    }) || filtered_entries_changed(b.market_orders.iter(), c.market_orders.iter(), |&id, o| {
        id == fix.b_order_id || o.owner_id == CHAR_B_ID || o.owner_id == CORP_B_ID
    }) || filtered_entries_changed(b.contracts.iter(), c.contracts.iter(), |&id, ct| {
        id == fix.b_contract_id
            || ct.issuer_id == CHAR_B_ID
            || ct.issuer_corp_id == CORP_B_ID
            || ct.assignee_id == CHAR_B_ID
            || ct.assignee_id == CORP_B_ID
    }) || filtered_entries_changed(b.industry_jobs.iter(), c.industry_jobs.iter(), |&id, j| {
        id == fix.b_job_id
            || j.installer_id == CHAR_B_ID
            || j.facility_id == CORP_B_STRUCTURE_ID
    }) || filtered_entries_changed(b.bookmarks.iter(), c.bookmarks.iter(), |&id, bm| {
        id == fix.b_bookmark_id || bm.owner_id == CHAR_B_ID || bm.owner_id == CORP_B_ID
    }) || filtered_entries_changed(
        b.bookmark_folders.iter(),
        c.bookmark_folders.iter(),
        |_, f| f.owner_id == CHAR_B_ID || f.owner_id == CORP_B_ID,
    ) || owner_notes_b_slice(b) != owner_notes_b_slice(c)
        || filtered_entries_changed(b.char_notes.iter(), c.char_notes.iter(), |k, _| {
        k.0 == CHAR_B_ID
    }) || filtered_entries_changed(
        b.mail_assigned_labels.iter(),
        c.mail_assigned_labels.iter(),
        |k, _| k.0 == CHAR_B_ID,
    ) || filtered_entries_changed(
        b.planetary_colonies.iter(),
        c.planetary_colonies.iter(),
        |k, _| k.0 == CHAR_B_ID,
    ) || filtered_entries_changed(
        b.planetary_colony_layouts.iter(),
        c.planetary_colony_layouts.iter(),
        |k, _| k.0 == CHAR_B_ID,
    ) || filtered_entries_changed(b.corp_projects.iter(), c.corp_projects.iter(), |&id, p| {
        id == fix.b_corp_project_id || p.corporation_id == CORP_B_ID
    }) || filtered_entries_changed(b.corp_vote_cases.iter(), c.corp_vote_cases.iter(), |_, v| {
        v.corporation_id == CORP_B_ID
    }) || filtered_entries_changed(
        b.corp_vote_ballots.iter(),
        c.corp_vote_ballots.iter(),
        |k, _| k.1 == CHAR_B_ID,
    ) || filtered_entries_changed(
        b.insurance_contracts.iter(),
        c.insurance_contracts.iter(),
        |_, ins| ins.owner_id == CHAR_B_ID || ins.owner_id == CORP_B_ID,
    ) || filtered_entries_changed(
        b.corp_wallet_journals.iter(),
        c.corp_wallet_journals.iter(),
        |k, _| k.0 == CORP_B_ID,
    ) || filtered_entries_changed(
        b.corp_wallet_transactions.iter(),
        c.corp_wallet_transactions.iter(),
        |k, _| k.0 == CORP_B_ID,
    ) || filtered_entries_changed(
        b.corp_lp_balances.iter(),
        c.corp_lp_balances.iter(),
        |k, _| k.0 == CORP_B_ID,
    ) || filtered_entries_changed(
        b.corp_role_assignments.iter(),
        c.corp_role_assignments.iter(),
        |k, _| k.0 == CORP_B_ID || k.1 == CHAR_B_ID,
    ) || filtered_entries_changed(
        b.corp_member_titles.iter(),
        c.corp_member_titles.iter(),
        |k, _| k.0 == CORP_B_ID || k.1 == CHAR_B_ID,
    ) || filtered_entries_changed(b.entity_kv.iter(), c.entity_kv.iter(), |k, _| {
        b_entity_ids.contains(&k.entity_id)
    }) {
        return true;
    }

    // Mail / wallet journal / mail state sets / timestamps for Target B.
    let mail_is_b = |m: &&MailRecord| {
        m.message_id == fix.b_mail_id
            || m.recipient_id == CHAR_B_ID
            || m.sender_id == CHAR_B_ID
            || m.recipient_id == CORP_B_ID
            || m.recipient_id == ALLIANCE_B_ID
            || m.recipient_id == fix.b_mail_id
    };
    let b_mails_before: Vec<_> = sb_mails.iter().filter(mail_is_b).collect();
    let b_mails_after: Vec<_> = sc_mails.iter().filter(mail_is_b).collect();
    if b_mails_before != b_mails_after {
        return true;
    }
    let mut b_mail_ids: BTreeSet<i64> = [fix.b_mail_id].into_iter().collect();
    for m in b_mails_before.iter().chain(b_mails_after.iter()) {
        b_mail_ids.insert(m.message_id);
    }
    if filtered_entries_changed(
        sb_mail_timestamps.iter(),
        sc_mail_timestamps.iter(),
        |id, _| b_mail_ids.contains(id),
    ) {
        return true;
    }
    let b_read_before: BTreeSet<(i64, i64)> = b
        .read_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    let b_read_after: BTreeSet<(i64, i64)> = c
        .read_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    let b_del_before: BTreeSet<(i64, i64)> = b
        .deleted_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    let b_del_after: BTreeSet<(i64, i64)> = c
        .deleted_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    let b_trash_before: BTreeSet<(i64, i64)> = b
        .trashed_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    let b_trash_after: BTreeSet<(i64, i64)> = c
        .trashed_mails
        .iter()
        .copied()
        .filter(|(cid, _)| *cid == CHAR_B_ID)
        .collect();
    if b_read_before != b_read_after
        || b_del_before != b_del_after
        || b_trash_before != b_trash_after
    {
        return true;
    }
    let b_journal_before: Vec<_> = sb_wallet_journal
        .iter()
        .filter(|e| e.character_id == CHAR_B_ID || e.character_id == CORP_B_ID)
        .collect();
    let b_journal_after: Vec<_> = sc_wallet_journal
        .iter()
        .filter(|e| e.character_id == CHAR_B_ID || e.character_id == CORP_B_ID)
        .collect();
    if b_journal_before != b_journal_after {
        return true;
    }
    let b_journal_ids: BTreeSet<i64> = b_journal_before
        .iter()
        .chain(b_journal_after.iter())
        .map(|e| e.entry_id)
        .collect();
    if filtered_entries_changed(
        sb_wallet_journal_timestamps.iter(),
        sc_wallet_journal_timestamps.iter(),
        |id, _| b_journal_ids.contains(id),
    ) || filtered_entries_changed(
        sb_wallet_journal_meta.iter(),
        sc_wallet_journal_meta.iter(),
        |id, m| {
            b_journal_ids.contains(id)
                || m.first_party_id == CHAR_B_ID
                || m.first_party_id == CORP_B_ID
                || m.first_party_id == ALLIANCE_B_ID
                || m.second_party_id == CHAR_B_ID
                || m.second_party_id == CORP_B_ID
                || m.second_party_id == ALLIANCE_B_ID
        },
    ) {
        return true;
    }

    // 4. Exhaustive remaining Target B snapshot tables & sub-stores (verified via compiler destructuring).
    // Check all positive Target B item/entity keys, per-char negative keys (-8_800_000 - CHAR_B_ID),
    // and every negative-key JSON/text side-store (< 0: -7_700_001, -7_700_002, -10_000_003..015, etc.).
    fn custom_names_b_slice<'a>(
        iter: impl IntoIterator<Item = (&'a i64, &'a String)>,
        b_item_ids: &BTreeSet<i64>,
        b_all_ids: &[i64],
    ) -> BTreeMap<i64, Value> {
        let mut out: BTreeMap<i64, Value> = BTreeMap::new();
        for (&k, v) in iter {
            if b_item_ids.contains(&k) || k == -8_800_000_i64 - CHAR_B_ID {
                out.insert(k, Value::String(v.clone()));
            } else if k < 0 {
                if let Some(sliced) = note_or_json_b_slice(Some(v), b_all_ids) {
                    out.insert(k, sliced);
                }
            }
        }
        out
    }
    if custom_names_b_slice(sb_item_custom_names.iter(), &b_item_ids, &b_all_ids)
        != custom_names_b_slice(sc_item_custom_names.iter(), &b_item_ids, &b_all_ids)
    {
        return true;
    }
    if filtered_entries_changed(
        sb_service_state_blobs.iter(),
        sc_service_state_blobs.iter(),
        |k, v| text_mentions_any_id(k, &b_all_ids) || text_mentions_any_id(v, &b_all_ids),
    ) || filtered_entries_changed(
        sb_quasar_state_blobs.iter(),
        sc_quasar_state_blobs.iter(),
        |k, v| text_mentions_any_id(k, &b_all_ids) || text_mentions_any_id(v, &b_all_ids),
    ) || sb_char_research_agents.get(&CHAR_B_ID) != sc_char_research_agents.get(&CHAR_B_ID)
        || filtered_entries_changed(
            sb_dungeon_runtime.instances.iter(),
            sc_dungeon_runtime.instances.iter(),
            |_, inst| {
                inst.owner_char_id == CHAR_B_ID
                    || inst
                        .entry_gate_ball_id
                        .is_some_and(|id| b_item_ids.contains(&id))
            },
        )
        || filtered_entries_changed(
            sb_dungeon_runtime.homefront_runtimes.iter(),
            sc_dungeon_runtime.homefront_runtimes.iter(),
            |_, hf| {
                hf.entered_pilots.contains_key(&CHAR_B_ID)
                    || hf.contributions.contains_key(&CHAR_B_ID)
                    || hf.spawned_ball_ids.iter().any(|id| b_item_ids.contains(id))
            },
        )
        || filtered_entries_changed(
            sb_mining_and_ess_state.ess_reserve_pulses.iter(),
            sc_mining_and_ess_state.ess_reserve_pulses.iter(),
            |_, st| st.reserve_linked_character_id == Some(CHAR_B_ID),
        )
        || filtered_entries_changed(
            sb_mining_and_ess_state.phased_asteroid_fields.iter(),
            sc_mining_and_ess_state.phased_asteroid_fields.iter(),
            |_, st| {
                st.connected_ship_ids.iter().any(|id| b_item_ids.contains(id))
                    || st.asteroid_ids.iter().any(|id| b_item_ids.contains(id))
            },
        )
        || sb_mining_and_ess_state
            .moon_fractured_fields
            .iter()
            .filter(|entry| b_struct_ids.contains(&entry.0) || b_item_ids.contains(&entry.0))
            .collect::<Vec<_>>()
            != sc_mining_and_ess_state
                .moon_fractured_fields
                .iter()
                .filter(|entry| b_struct_ids.contains(&entry.0) || b_item_ids.contains(&entry.0))
                .collect::<Vec<_>>()
        || filtered_entries_changed(
            sb_mining_and_ess_state.mining_ledger_waste.iter(),
            sc_mining_and_ess_state.mining_ledger_waste.iter(),
            |&idx, _| {
                sb_mining_ledger
                    .get(idx)
                    .is_some_and(|r| r.character_id == CHAR_B_ID)
                    || sc_mining_ledger
                        .get(idx)
                        .is_some_and(|r| r.character_id == CHAR_B_ID)
            },
        )
        || filtered_entries_changed(
            sb_insurgency_complex_ops.outpost_raids.iter(),
            sc_insurgency_complex_ops.outpost_raids.iter(),
            |_, r| {
                r.occupants
                    .iter()
                    .any(|(cid, sid)| *cid == CHAR_B_ID || b_item_ids.contains(sid))
                    || r.mordus_defender_ball_id
                        .is_some_and(|id| b_item_ids.contains(&id))
                    || r.pirate_defender_ball_id
                        .is_some_and(|id| b_item_ids.contains(&id))
            },
        )
        || filtered_entries_changed(
            sb_insurgency_complex_ops.mining_ambushes.iter(),
            sc_insurgency_complex_ops.mining_ambushes.iter(),
            |_, a| {
                b_item_ids.contains(&a.rorqual_ball_id)
                    || a.pirate_tackler_ids.iter().any(|id| b_item_ids.contains(id))
                    || a.ore_defender_ids.iter().any(|id| b_item_ids.contains(id))
            },
        )
        || filtered_entries_changed(
            sb_insurgency_complex_ops.subpoint_remainders.iter(),
            sc_insurgency_complex_ops.subpoint_remainders.iter(),
            |k, _| text_mentions_any_id(k, &b_all_ids),
        )
        || filtered_entries_changed(
            sb_insurgency_complex_ops.named_roamer_rolls.iter(),
            sc_insurgency_complex_ops.named_roamer_rolls.iter(),
            |k, _| text_mentions_any_id(k, &b_all_ids),
        )
    {
        return true;
    }
    if filtered_entries_changed(sb_sessions.iter(), sc_sessions.iter(), |_, s| {
        s.user_id == USER_B_ID || s.character_id == Some(CHAR_B_ID)
    }) || filtered_entries_changed(
        sb_active_missions.iter(),
        sc_active_missions.iter(),
        |(cid, _), rec| *cid == CHAR_B_ID || rec.character_id == CHAR_B_ID,
    ) || sb_mission_tally.get(&CHAR_B_ID) != sc_mission_tally.get(&CHAR_B_ID)
        || sb_last_mission_decline_times.get(&CHAR_B_ID)
            != sc_last_mission_decline_times.get(&CHAR_B_ID)
        || sb_character_last_jump_unix.get(&CHAR_B_ID)
            != sc_character_last_jump_unix.get(&CHAR_B_ID)
        || sb_character_cspa_charges.get(&CHAR_B_ID) != sc_character_cspa_charges.get(&CHAR_B_ID)
        || sb_character_login_stats.get(&CHAR_B_ID) != sc_character_login_stats.get(&CHAR_B_ID)
        || sb_fw_corp_enlisted_on.get(&CORP_B_ID) != sc_fw_corp_enlisted_on.get(&CORP_B_ID)
        || sb_zarzakh_gate_locks.get(&CHAR_B_ID) != sc_zarzakh_gate_locks.get(&CHAR_B_ID)
    {
        return true;
    }
    if filtered_entries_changed(sb_fleets.iter(), sc_fleets.iter(), |&fid, f| {
        fid == fix.b_fleet_id
            || f.boss_id == CHAR_B_ID
            || f.members.contains(&CHAR_B_ID)
            || f.invites.contains(&CHAR_B_ID)
    }) || filtered_entries_changed(sb_killmails.iter(), sc_killmails.iter(), |&kid, km| {
        kid == fix.b_killmail_id
            || km.victim_character_id == CHAR_B_ID
            || km.attacker_character_id == CHAR_B_ID
            || km.victim_corp_id == CORP_B_ID
            || km.attacker_corp_id == CORP_B_ID
    }) || filtered_entries_changed(sb_kill_rights.iter(), sc_kill_rights.iter(), |_, kr| {
        kr.owner_id == CHAR_B_ID || kr.target_id == CHAR_B_ID
    }) {
        return true;
    }

    // Chat channels & ACLs touching Target B; normalize shared local-chat (`solarsystemid2:*`)
    // memberships to ignore the caller (`CHAR_A_ID`) joining/leaving while still verifying every
    // Target B channel field and Target B membership.
    let mut b_chan_keys: BTreeSet<String> = BTreeSet::new();
    for (k, ch) in sb_chat_channels.iter().chain(sc_chat_channels.iter()) {
        if ch.owner_id == Some(CHAR_B_ID)
            || ch.owner_id == Some(CORP_B_ID)
            || ch.owner_id == Some(ALLIANCE_B_ID)
            || ch.members.contains(&CHAR_B_ID)
            || ch.operators.contains(&CHAR_B_ID)
        {
            b_chan_keys.insert(k.clone());
        }
    }
    let [b_chans_before, b_chans_after] = [sb_chat_channels, sc_chat_channels].map(|channels| {
        channels
            .iter()
            .filter(|(k, _)| b_chan_keys.contains(*k))
            .map(|(k, ch)| {
                let mut norm = ch.clone();
                if k.starts_with("solarsystemid2:")
                    && !matches!(
                        norm.owner_id,
                        Some(CHAR_B_ID) | Some(CORP_B_ID) | Some(ALLIANCE_B_ID)
                    )
                {
                    norm.members.remove(&CHAR_A_ID);
                }
                (k.clone(), norm)
            })
            .collect::<BTreeMap<_, _>>()
    });
    if b_chans_before != b_chans_after
        || filtered_entries_changed(
            sb_chat_channel_acl.iter(),
            sc_chat_channel_acl.iter(),
            |k, acl| {
                b_chan_keys.contains(k)
                    || acl.allow_list.contains(&CHAR_B_ID)
                    || acl.deny_list.contains(&CHAR_B_ID)
                    || acl.muted_until.contains_key(&CHAR_B_ID)
            },
        )
    {
        return true;
    }

    // Bookmark folders, subfolders, expiries, folder ACLs, access groups, and known-by state.
    let mut b_bm_ids: BTreeSet<i64> = [fix.b_bookmark_id].into_iter().collect();
    for (&id, bm) in sb_bookmarks.iter().chain(sc_bookmarks.iter()) {
        if id == fix.b_bookmark_id || bm.owner_id == CHAR_B_ID || bm.owner_id == CORP_B_ID {
            b_bm_ids.insert(id);
        }
    }
    let mut b_folder_ids: BTreeSet<i64> = BTreeSet::new();
    let mut b_group_ids: BTreeSet<i64> = BTreeSet::new();
    for (&fid, f) in sb_bookmark_folders.iter().chain(sc_bookmark_folders.iter()) {
        if f.owner_id == CHAR_B_ID
            || f.owner_id == CORP_B_ID
            || f.corporation_id == Some(CORP_B_ID)
        {
            b_folder_ids.insert(fid);
            if let Some(g) = f.admin_group_id {
                b_group_ids.insert(g);
            }
            if let Some(g) = f.manage_group_id {
                b_group_ids.insert(g);
            }
            if let Some(g) = f.use_group_id {
                b_group_ids.insert(g);
            }
            if let Some(g) = f.view_group_id {
                b_group_ids.insert(g);
            }
        }
    }
    if filtered_entries_changed(
        sb_bookmark_folders.iter(),
        sc_bookmark_folders.iter(),
        |id, _| b_folder_ids.contains(id),
    ) || filtered_entries_changed(
        sb_bookmark_subfolders.iter(),
        sc_bookmark_subfolders.iter(),
        |bm_id, fid| b_bm_ids.contains(bm_id) || b_folder_ids.contains(fid),
    ) || filtered_entries_changed(
        sb_bookmark_subfolder_parents.iter(),
        sc_bookmark_subfolder_parents.iter(),
        |child, parent| b_folder_ids.contains(child) || b_folder_ids.contains(parent),
    ) || filtered_entries_changed(
        sb_bookmark_expiries.iter(),
        sc_bookmark_expiries.iter(),
        |id, _| b_bm_ids.contains(id),
    ) || filtered_entries_changed(
        sb_bookmark_folder_acls.iter(),
        sc_bookmark_folder_acls.iter(),
        |fid, members| {
            b_folder_ids.contains(fid)
                || members.contains(&CHAR_B_ID)
                || members.contains(&CORP_B_ID)
                || members.contains(&ALLIANCE_B_ID)
        },
    ) || filtered_entries_changed(sb_access_groups.iter(), sc_access_groups.iter(), |gid, members| {
        b_group_ids.contains(gid)
            || members.contains(&CHAR_B_ID)
            || members.contains(&CORP_B_ID)
            || members.contains(&ALLIANCE_B_ID)
    }) || filtered_entries_changed(
        sb_bookmark_folder_known_by.iter(),
        sc_bookmark_folder_known_by.iter(),
        |(fid, cid), _| *cid == CHAR_B_ID || b_folder_ids.contains(fid),
    ) {
        return true;
    }

    // Calendar, mailing lists, contacts, notes, and social records for Target B.
    if filtered_entries_changed(sb_mailing_lists.iter(), sc_mailing_lists.iter(), |_, ml| {
        ml.owner_id == CHAR_B_ID
            || ml.owner_id == CORP_B_ID
            || ml.owner_id == ALLIANCE_B_ID
            || ml.members.contains_key(&CHAR_B_ID)
            || ml.entity_access.contains_key(&CHAR_B_ID)
            || ml.entity_access.contains_key(&CORP_B_ID)
    }) || filtered_entries_changed(sb_calendar_events.iter(), sc_calendar_events.iter(), |_, ev| {
        ev.owner_id == CHAR_B_ID
            || ev.owner_id == CORP_B_ID
            || ev.owner_id == ALLIANCE_B_ID
            || ev.creator_id == CHAR_B_ID
            || ev.responses.contains_key(&CHAR_B_ID)
    }) || sb_char_contacts.get(&CHAR_B_ID).map(|v| {
        v.iter()
            .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
            .collect::<Vec<_>>()
    }) != sc_char_contacts.get(&CHAR_B_ID).map(|v| {
        v.iter()
            .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
            .collect::<Vec<_>>()
    }) || sb_char_contact_labels.get(&CHAR_B_ID) != sc_char_contact_labels.get(&CHAR_B_ID)
        || sb_char_contact_notifications.get(&CHAR_B_ID).map(|v| {
            v.iter()
                .map(|n| (n.notification_id, n.sender_character_id, n.send_date_unix, n.standing_level.to_bits(), &n.message))
                .collect::<Vec<_>>()
        }) != sc_char_contact_notifications.get(&CHAR_B_ID).map(|v| {
            v.iter()
                .map(|n| (n.notification_id, n.sender_character_id, n.send_date_unix, n.standing_level.to_bits(), &n.message))
                .collect::<Vec<_>>()
        })
        || sb_corp_contacts.get(&CORP_B_ID).map(|v| {
            v.iter()
                .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
                .collect::<Vec<_>>()
        }) != sc_corp_contacts.get(&CORP_B_ID).map(|v| {
            v.iter()
                .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
                .collect::<Vec<_>>()
        })
        || sb_corp_contact_labels.get(&CORP_B_ID) != sc_corp_contact_labels.get(&CORP_B_ID)
        || sb_alliance_contacts.get(&ALLIANCE_B_ID).map(|v| {
            v.iter()
                .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
                .collect::<Vec<_>>()
        }) != sc_alliance_contacts.get(&ALLIANCE_B_ID).map(|v| {
            v.iter()
                .map(|c| (c.contact_id, &c.contact_type, c.standing.to_bits(), c.is_watched, c.is_blocked, &c.label_ids))
                .collect::<Vec<_>>()
        })
        || sb_alliance_contact_labels.get(&ALLIANCE_B_ID)
            != sc_alliance_contact_labels.get(&ALLIANCE_B_ID)
        || filtered_entries_changed(sb_char_notes.iter(), sc_char_notes.iter(), |(owner, _), _| {
            *owner == CHAR_B_ID
        })
    {
        return true;
    }

    // Mining ledger, moon extractions, observers, corp container logs, project contributors, market meta/txs, corp votes.
    let b_mining_before: Vec<_> = sb_mining_ledger.iter().filter(|r| r.character_id == CHAR_B_ID).collect();
    let b_mining_after: Vec<_> = sc_mining_ledger.iter().filter(|r| r.character_id == CHAR_B_ID).collect();
    let mut b_order_ids: BTreeSet<i64> = [fix.b_order_id].into_iter().collect();
    for (&oid, o) in sb_market_orders.iter().chain(sc_market_orders.iter()) {
        if o.owner_id == CHAR_B_ID || o.owner_id == CORP_B_ID {
            b_order_ids.insert(oid);
        }
    }
    let mut b_proj_ids: BTreeSet<i64> = [fix.b_corp_project_id].into_iter().collect();
    for (&pid, p) in sb_corp_projects.iter().chain(sc_corp_projects.iter()) {
        if p.corporation_id == CORP_B_ID {
            b_proj_ids.insert(pid);
        }
    }
    let mut b_vote_case_ids: BTreeSet<i64> = BTreeSet::new();
    for (&cid, vc) in sb_corp_vote_cases.iter().chain(sc_corp_vote_cases.iter()) {
        if vc.corporation_id == CORP_B_ID {
            b_vote_case_ids.insert(cid);
        }
    }
    if b_mining_before != b_mining_after
        || filtered_entries_changed(sb_moon_extractions.iter(), sc_moon_extractions.iter(), |id, _| {
            b_struct_ids.contains(id)
        })
        || filtered_entries_changed(sb_auto_moon_miners.iter(), sc_auto_moon_miners.iter(), |id, _| {
            b_struct_ids.contains(id)
        })
        || filtered_entries_changed(
            sb_observer_mining_ledger.iter(),
            sc_observer_mining_ledger.iter(),
            |obs_id, recs| {
                b_struct_ids.contains(obs_id)
                    || recs
                        .iter()
                        .any(|r| r.character_id == CHAR_B_ID || r.recorded_corporation_id == CORP_B_ID)
            },
        )
        || sb_corp_container_logs.get(&CORP_B_ID) != sc_corp_container_logs.get(&CORP_B_ID)
        || filtered_entries_changed(
            sb_corp_container_logs.iter(),
            sc_corp_container_logs.iter(),
            |&cid, logs| {
                cid == CORP_B_ID
                    || logs
                        .iter()
                        .any(|l| l.character_id == CHAR_B_ID || l.container_id == fix.b_cont_id)
            },
        )
        || filtered_entries_changed(
            sb_corp_project_contributors.iter(),
            sc_corp_project_contributors.iter(),
            |pid, entries| {
                b_proj_ids.contains(pid) || entries.iter().any(|e| e.character_id == CHAR_B_ID)
            },
        )
        || filtered_entries_changed(sb_market_order_meta.iter(), sc_market_order_meta.iter(), |oid, m| {
            b_order_ids.contains(oid) || m.issued_by == Some(CHAR_B_ID)
        })
        || filtered_entries_changed(
            sb_market_transactions.iter(),
            sc_market_transactions.iter(),
            |_, tx| {
                tx.buyer_id == CHAR_B_ID
                    || tx.buyer_id == CORP_B_ID
                    || tx.seller_id == CHAR_B_ID
                    || tx.seller_id == CORP_B_ID
            },
        )
        || filtered_entries_changed(sb_corp_vote_cases.iter(), sc_corp_vote_cases.iter(), |_, vc| {
            vc.corporation_id == CORP_B_ID
        })
        || filtered_entries_changed(
            sb_corp_vote_ballots.iter(),
            sc_corp_vote_ballots.iter(),
            |(case_id, voter_id), _| *voter_id == CHAR_B_ID || b_vote_case_ids.contains(case_id),
        )
    {
        return true;
    }

    // Idempotency, wormhole polarizations, system ESS/sov attribution, war/alliance/fw/sov/insurgency Target B state.
    let b_system_slice = |sys: &eve_db::SystemDynamicState| {
        (
            sys.ess_linked_character_id == Some(CHAR_B_ID),
            sys.ess_main_bank_contributors.get(&CHAR_B_ID).copied(),
            sys.sov_alliance_id == Some(ALLIANCE_B_ID),
        )
    };
    let sys_b_before: BTreeMap<_, _> = sb_system_states
        .iter()
        .map(|(&sid, s)| (sid, b_system_slice(s)))
        .collect();
    let sys_b_after: BTreeMap<_, _> = sc_system_states
        .iter()
        .map(|(&sid, s)| (sid, b_system_slice(s)))
        .collect();
    let fw_wars_b_before: Vec<_> = sb_fw_wars
        .iter()
        .filter(|&&(a, b_id)| a == CORP_B_ID || a == ALLIANCE_B_ID || b_id == CORP_B_ID || b_id == ALLIANCE_B_ID)
        .collect();
    let fw_wars_b_after: Vec<_> = sc_fw_wars
        .iter()
        .filter(|&&(a, b_id)| a == CORP_B_ID || a == ALLIANCE_B_ID || b_id == CORP_B_ID || b_id == ALLIANCE_B_ID)
        .collect();
    let idem_b_before: BTreeSet<_> = sb_committed_idempotency_keys
        .iter()
        .filter(|(cid, _)| *cid == CHAR_B_ID || *cid == USER_B_ID as i64)
        .collect();
    let idem_b_after: BTreeSet<_> = sc_committed_idempotency_keys
        .iter()
        .filter(|(cid, _)| *cid == CHAR_B_ID || *cid == USER_B_ID as i64)
        .collect();
    if sys_b_before != sys_b_after
        || fw_wars_b_before != fw_wars_b_after
        || idem_b_before != idem_b_after
        || filtered_entries_changed(
            sb_wh_polarizations.iter(),
            sc_wh_polarizations.iter(),
            |&(_, cid), _| cid == CHAR_B_ID,
        )
        || sb_alliance_war_state.alliances.get(&ALLIANCE_B_ID)
            != sc_alliance_war_state.alliances.get(&ALLIANCE_B_ID)
        || sb_war_state.wars.get(&fix.b_war_id) != sc_war_state.wars.get(&fix.b_war_id)
        || sb_war_state.blocked_mutual_invites.get(&CHAR_B_ID)
            != sc_war_state.blocked_mutual_invites.get(&CHAR_B_ID)
        || sb_war_state.blocked_mutual_invites.get(&CORP_B_ID)
            != sc_war_state.blocked_mutual_invites.get(&CORP_B_ID)
        || filtered_entries_changed(
            sb_war_state.mutual_invites.iter(),
            sc_war_state.mutual_invites.iter(),
            |&(a, b_id), _| b_entity_ids.contains(&a) || b_entity_ids.contains(&b_id),
        )
        || filtered_entries_changed(
            sb_war_state.ally_expires_unix.iter(),
            sc_war_state.ally_expires_unix.iter(),
            |&(wid, id), _| wid == fix.b_war_id || b_entity_ids.contains(&id),
        )
        || sb_war_state
            .negotiations
            .iter()
            .filter(|n| n.war_id == fix.b_war_id || n.from_corp == CORP_B_ID)
            .collect::<Vec<_>>()
            != sc_war_state
                .negotiations
                .iter()
                .filter(|n| n.war_id == fix.b_war_id || n.from_corp == CORP_B_ID)
                .collect::<Vec<_>>()
        || sb_war_state
            .peace_treaties
            .iter()
            .filter(|p| p.war_id == fix.b_war_id || p.corp_a == CORP_B_ID || p.corp_b == CORP_B_ID)
            .collect::<Vec<_>>()
            != sc_war_state
                .peace_treaties
                .iter()
                .filter(|p| p.war_id == fix.b_war_id || p.corp_a == CORP_B_ID || p.corp_b == CORP_B_ID)
                .collect::<Vec<_>>()
    {
        return true;
    }

    // FW runtime, Sov runtime, and Insurgency runtime entries mentioning Target B.
    let fw_part_b = |m: &HashMap<i64, Vec<i64>>| -> BTreeMap<i64, bool> {
        m.iter()
            .filter_map(|(&k, ps)| ps.contains(&CHAR_B_ID).then_some((k, true)))
            .collect()
    };
    let sov_b_slice = |s: &eve_db::durable_domain_types::PersistedSovState| {
        (
            b_struct_ids
                .iter()
                .filter_map(|&id| s.sov_hubs.get(&id).map(|v| (id, v.clone())))
                .collect::<BTreeMap<_, _>>(),
            s.mercenary_dens
                .iter()
                .filter(|(_, d)| d.owner_char_id == CHAR_B_ID || d.planet_id == fix.b_planet_id)
                .map(|(&k, v)| (k, v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let ins_b_slice = |i: &eve_db::durable_domain_types::PersistedInsurgencyState| {
        i.campaign_contributions
            .iter()
            .map(|(&cid, v)| {
                (
                    cid,
                    v.iter()
                        .filter(|&&(ch, _, _)| ch == CHAR_B_ID)
                        .copied()
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    if fw_part_b(&sb_fw_runtime.battlefield_participants)
        != fw_part_b(&sc_fw_runtime.battlefield_participants)
        || sov_b_slice(sb_sov_runtime) != sov_b_slice(sc_sov_runtime)
        || ins_b_slice(sb_insurgency_runtime) != ins_b_slice(sc_insurgency_runtime)
    {
        return true;
    }

    // Crimewatch aggressions & combat_crimewatch_state for Target B.
    let cw_slice = |cw: &PersistedCombatCrimewatchState| {
        (
            cw.sec_status_transactions
                .get(&CHAR_B_ID)
                .map(|v| {
                    v.iter()
                        .map(|t| {
                            (
                                t.timestamp_unix,
                                t.old_sec.to_bits(),
                                t.new_sec.to_bits(),
                                t.reason.clone(),
                                t.event_type_id,
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            cw.safety_levels.get(&CHAR_B_ID).copied(),
            cw.drone_settings.get(&CHAR_B_ID).copied(),
            cw.limited_engagements
                .iter()
                .flat_map(|(&a, inner)| {
                    inner.iter().filter_map(move |(&b_id, &u)| {
                        (a == CHAR_B_ID || b_id == CHAR_B_ID).then_some(((a, b_id), u))
                    })
                })
                .collect::<BTreeMap<_, _>>(),
            cw.npc_timers.get(&CHAR_B_ID).copied(),
            cw.disapproval_flags.get(&CHAR_B_ID).copied(),
            cw.duel_challenges
                .iter()
                .filter(|(&(a, b_id), _)| a == CHAR_B_ID || b_id == CHAR_B_ID)
                .map(|(&k, &v)| (k, v))
                .collect::<BTreeMap<_, _>>(),
            cw.corp_aggression_settings.get(&CORP_B_ID).copied(),
            [CHAR_B_ID, CORP_B_ID, ALLIANCE_B_ID]
                .into_iter()
                .filter_map(|id| cw.player_bounty_pool.get(&id).map(|v| (id, v.to_bits())))
                .collect::<BTreeMap<_, _>>(),
            [CHAR_B_ID, CORP_B_ID, ALLIANCE_B_ID]
                .into_iter()
                .filter_map(|id| cw.bounty_hunter_claimed.get(&id).map(|v| (id, v.to_bits())))
                .collect::<BTreeMap<_, _>>(),
            cw.detailed_killmails
                .iter()
                .filter(|(&kid, km)| {
                    kid == fix.b_killmail_id
                        || km.victim_character_id == CHAR_B_ID
                        || km.victim_corporation_id == CORP_B_ID
                })
                .map(|(&kid, km)| {
                    (
                        kid,
                        (
                            km.kill_time_unix,
                            km.victim_character_id,
                            km.attackers.len(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
    };
    if filtered_entries_changed(
        sb_crimewatch_aggressions.iter(),
        sc_crimewatch_aggressions.iter(),
        |&(a, v), _| a == CHAR_B_ID || v == CHAR_B_ID,
    ) || cw_slice(sb_combat_crimewatch_state) != cw_slice(sc_combat_crimewatch_state)
    {
        return true;
    }

    // SP6 lifecycle, PvE sites, CRAB beacons, Abyssal, Dungeon editor, FOB, PvP, Tourney, Deployables, Superweapons.
    if sb_sp6_lifecycle.safe_logoffs.get(&CHAR_B_ID) != sc_sp6_lifecycle.safe_logoffs.get(&CHAR_B_ID)
        || sb_sp6_lifecycle.pvp_timers_until.get(&CHAR_B_ID)
            != sc_sp6_lifecycle.pvp_timers_until.get(&CHAR_B_ID)
        || sb_sp6_lifecycle.npc_timers_until.get(&CHAR_B_ID)
            != sc_sp6_lifecycle.npc_timers_until.get(&CHAR_B_ID)
        || sb_sp6_lifecycle.weapon_timers_until.get(&CHAR_B_ID)
            != sc_sp6_lifecycle.weapon_timers_until.get(&CHAR_B_ID)
        || sb_sp6_lifecycle.criminal_timers_until.get(&CHAR_B_ID)
            != sc_sp6_lifecycle.criminal_timers_until.get(&CHAR_B_ID)
        || sb_sp6_lifecycle.abyssal_sessions.get(&CHAR_B_ID)
            != sc_sp6_lifecycle.abyssal_sessions.get(&CHAR_B_ID)
        || b_entity_ids.iter().any(|id| {
            sb_sp6_lifecycle.disconnected_ships.get(id) != sc_sp6_lifecycle.disconnected_ships.get(id)
                || sb_sp6_lifecycle.self_destructs.get(id) != sc_sp6_lifecycle.self_destructs.get(id)
                || sb_sp6_lifecycle.ship_combat_hp.get(id) != sc_sp6_lifecycle.ship_combat_hp.get(id)
                || sb_sp6_lifecycle.space_object_meta.get(id)
                    != sc_sp6_lifecycle.space_object_meta.get(id)
                || sb_sp6_lifecycle.item_velocities.get(id) != sc_sp6_lifecycle.item_velocities.get(id)
        })
        || filtered_entries_changed(
            sb_sp6_lifecycle.active_scanner_probes.iter(),
            sc_sp6_lifecycle.active_scanner_probes.iter(),
            |_, p| p.owner_char_id == CHAR_B_ID || b_entity_ids.contains(&p.ship_id),
        )
        || (sb_sp6_lifecycle.server_runtime_json != sc_sp6_lifecycle.server_runtime_json
            && (sb_sp6_lifecycle
                .server_runtime_json
                .as_deref()
                .is_some_and(|s| text_mentions_any_id(s, &b_entity_ids))
                || sc_sp6_lifecycle
                    .server_runtime_json
                    .as_deref()
                    .is_some_and(|s| text_mentions_any_id(s, &b_entity_ids))))
    {
        return true;
    }
    if sb_pve_special_sites.incursion_lp_logs.get(&CHAR_B_ID)
        != sc_pve_special_sites.incursion_lp_logs.get(&CHAR_B_ID)
        || filtered_entries_changed(
            sb_crab_beacons.beacons.iter(),
            sc_crab_beacons.beacons.iter(),
            |_, bc| {
                bc.deployer_char_id == CHAR_B_ID
                    || bc.linked_char_id == Some(CHAR_B_ID)
                    || bc.linked_ship_id.is_some_and(|s| b_entity_ids.contains(&s))
            },
        )
        || filtered_entries_changed(
            sb_abyssal_state.pockets.iter(),
            sc_abyssal_state.pockets.iter(),
            |_, p| p.character_id == CHAR_B_ID || b_entity_ids.contains(&p.ship_id),
        )
        || filtered_entries_changed(
            sb_abyssal_state.traces.iter(),
            sc_abyssal_state.traces.iter(),
            |_, t| t.char_id == CHAR_B_ID,
        )
        || filtered_entries_changed(
            sb_abyssal_state.void_space_sessions.iter(),
            sc_abyssal_state.void_space_sessions.iter(),
            |_, v| v.char_id == CHAR_B_ID || b_entity_ids.contains(&v.ship_id),
        )
        || b_entity_ids.iter().any(|id| {
            sb_abyssal_state.deathzone_active_ships.contains(id)
                != sc_abyssal_state.deathzone_active_ships.contains(id)
        })
        || sb_dungeon_editor.active_editors.get(&CHAR_B_ID)
            != sc_dungeon_editor.active_editors.get(&CHAR_B_ID)
        || filtered_entries_changed(
            sb_fob_observatory_state.fob_combatants.iter(),
            sc_fob_observatory_state.fob_combatants.iter(),
            |_, set| set.contains(&CHAR_B_ID),
        )
        || sb_superweapon_runtime
            .active_cycles
            .iter()
            .filter(|c| c.char_id == CHAR_B_ID || b_entity_ids.contains(&c.ship_id))
            .collect::<Vec<_>>()
            != sc_superweapon_runtime
                .active_cycles
                .iter()
                .filter(|c| c.char_id == CHAR_B_ID || b_entity_ids.contains(&c.ship_id))
                .collect::<Vec<_>>()
        || sb_superweapon_runtime
            .warp_suppressions
            .iter()
            .filter(|w| b_entity_ids.contains(&w.ship_id))
            .collect::<Vec<_>>()
            != sc_superweapon_runtime
                .warp_suppressions
                .iter()
                .filter(|w| b_entity_ids.contains(&w.ship_id))
                .collect::<Vec<_>>()
    {
        return true;
    }

    let domain_scope = TargetBDomainScope {
        b_fleet_id: fix.b_fleet_id,
        b_contract_id: fix.b_contract_id,
        b_petition_id: fix.b_petition_id,
        b_entity_ids: &b_entity_ids,
        b_item_ids: &b_item_ids,
        b_struct_ids: &b_struct_ids,
        b_all_ids: &b_all_ids,
    };
    let stores_before = TargetBStoreRefs {
        pvp: sb_pvp_filament_state,
        tourney: sb_tourney_state,
        deployable: sb_deployable_state,
        node_graph: sb_node_graph_state,
        seasons: sb_seasons_and_campaigns,
        quasar: sb_quasar_commerce,
        fleet_ops: sb_fleet_and_social_ops,
        cluster: sb_cluster_and_runtime,
    };
    let stores_after = TargetBStoreRefs {
        pvp: sc_pvp_filament_state,
        tourney: sc_tourney_state,
        deployable: sc_deployable_state,
        node_graph: sc_node_graph_state,
        seasons: sc_seasons_and_campaigns,
        quasar: sc_quasar_commerce,
        fleet_ops: sc_fleet_and_social_ops,
        cluster: sc_cluster_and_runtime,
    };
    target_b_sub_stores_mutated(&domain_scope, &stores_before, &stores_after)
}

struct TargetBDomainScope<'a> {
    b_fleet_id: i64,
    b_contract_id: i64,
    b_petition_id: i64,
    b_entity_ids: &'a [i64],
    b_item_ids: &'a BTreeSet<i64>,
    b_struct_ids: &'a BTreeSet<i64>,
    b_all_ids: &'a [i64],
}

struct TargetBStoreRefs<'a> {
    pvp: &'a eve_db::service_domains::PvpFilamentStore,
    tourney: &'a eve_db::service_domains::TourneyStore,
    deployable: &'a eve_db::service_domains::MobileDeployableRuntimeState,
    node_graph: &'a eve_db::service_domains::NodeGraphAndProgressionStore,
    seasons: &'a eve_db::service_domains::SeasonsAndCampaignsStore,
    quasar: &'a eve_db::service_domains::QuasarCommerceStore,
    fleet_ops: &'a eve_db::service_domains::FleetAndSocialOpsStore,
    cluster: &'a eve_db::service_domains::ClusterAndRuntimeStore,
}

fn target_b_sub_stores_mutated(
    scope: &TargetBDomainScope<'_>,
    before: &TargetBStoreRefs<'_>,
    after: &TargetBStoreRefs<'_>,
) -> bool {
    let TargetBDomainScope {
        b_fleet_id,
        b_contract_id,
        b_petition_id,
        b_entity_ids,
        b_item_ids,
        b_struct_ids,
        b_all_ids,
    } = *scope;

    let (b_pvp_match_ids, b_pvp_ticket_ids) = {
        let mut match_ids = BTreeSet::new();
        let mut ticket_ids = BTreeSet::new();
        let mut collect = |pvp: &eve_db::service_domains::PvpFilamentStore| {
            for (&mid, m) in &pvp.active_matches {
                if m.team_a.contains(&CHAR_B_ID)
                    || m.team_b.contains(&CHAR_B_ID)
                    || m.origin_systems.contains_key(&CHAR_B_ID)
                    || m.winner_char_id == Some(CHAR_B_ID)
                    || m.exited_pilots.contains(&CHAR_B_ID)
                    || b_item_ids.contains(&m.end_gate_id)
                {
                    match_ids.insert(mid);
                }
            }
            for (&mid, teams) in &pvp.match_teams {
                if teams.iter().any(|t| t.contains(&CHAR_B_ID)) {
                    match_ids.insert(mid);
                }
            }
            for (&mid, eliminated) in &pvp.match_eliminated {
                if eliminated.contains(&CHAR_B_ID) {
                    match_ids.insert(mid);
                }
            }
            if let Some(&tid) = pvp.queue_tickets.get(&CHAR_B_ID) {
                ticket_ids.insert(tid);
            }
            for (&tid, &(cid, loc)) in &pvp.queue_refund_targets {
                if cid == CHAR_B_ID || b_item_ids.contains(&loc) || b_item_ids.contains(&tid) {
                    ticket_ids.insert(tid);
                }
            }
        };
        collect(before.pvp);
        collect(after.pvp);
        (match_ids, ticket_ids)
    };
    let pvp_b = |pvp: &eve_db::service_domains::PvpFilamentStore| {
        let eve_db::service_domains::PvpFilamentStore {
            queue,
            stats,
            event_stats,
            events: _,
            seeded_default_events: _,
            active_matches,
            match_teams,
            match_eliminated,
            match_arena_offsets,
            match_filament_types,
            hazard_ticks,
            queue_refund_targets,
            queue_tickets,
            next_ticket_id: _,
            origin_positions,
            next_match_id: _,
        } = pvp;
        (
            (
                queue.get(&CHAR_B_ID).cloned(),
                stats
                    .get(&CHAR_B_ID)
                    .map(|v| (v.0, v.1, v.2, v.3, v.4.to_bits())),
                event_stats
                    .iter()
                    .filter_map(|(&eid, m)| {
                        m.get(&CHAR_B_ID)
                            .map(|v| (eid, (v.0, v.1, v.2, v.3, v.4.to_bits())))
                    })
                    .collect::<BTreeMap<_, _>>(),
                active_matches
                    .iter()
                    .filter(|(&mid, m)| {
                        b_pvp_match_ids.contains(&mid)
                            || m.team_a.contains(&CHAR_B_ID)
                            || m.team_b.contains(&CHAR_B_ID)
                            || m.origin_systems.contains_key(&CHAR_B_ID)
                            || m.winner_char_id == Some(CHAR_B_ID)
                            || m.exited_pilots.contains(&CHAR_B_ID)
                            || b_item_ids.contains(&m.end_gate_id)
                    })
                    .map(|(&mid, m)| (mid, m.clone()))
                    .collect::<BTreeMap<_, _>>(),
                match_teams
                    .iter()
                    .filter(|(&mid, teams)| {
                        b_pvp_match_ids.contains(&mid)
                            || teams.iter().any(|t| t.contains(&CHAR_B_ID))
                    })
                    .map(|(&mid, teams)| (mid, teams.clone()))
                    .collect::<BTreeMap<_, _>>(),
                match_eliminated
                    .iter()
                    .filter(|(&mid, set)| {
                        b_pvp_match_ids.contains(&mid) || set.contains(&CHAR_B_ID)
                    })
                    .map(|(&mid, set)| (mid, set.iter().copied().collect::<BTreeSet<_>>()))
                    .collect::<BTreeMap<_, _>>(),
            ),
            (
                match_arena_offsets
                    .iter()
                    .filter(|(&mid, _)| b_pvp_match_ids.contains(&mid))
                    .map(|(&mid, p)| (mid, [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]))
                    .collect::<BTreeMap<_, _>>(),
                match_filament_types
                    .iter()
                    .filter(|(&mid, _)| b_pvp_match_ids.contains(&mid))
                    .map(|(&mid, &tid)| (mid, tid))
                    .collect::<BTreeMap<_, _>>(),
                hazard_ticks
                    .iter()
                    .filter(|(&sid, _)| b_item_ids.contains(&sid))
                    .map(|(&sid, &t)| (sid, t))
                    .collect::<BTreeMap<_, _>>(),
                queue_refund_targets
                    .iter()
                    .filter(|(&tid, &(cid, loc))| {
                        cid == CHAR_B_ID
                            || b_item_ids.contains(&loc)
                            || b_item_ids.contains(&tid)
                            || b_pvp_ticket_ids.contains(&tid)
                    })
                    .map(|(&tid, &val)| (tid, val))
                    .collect::<BTreeMap<_, _>>(),
                queue_tickets.get(&CHAR_B_ID).copied(),
                origin_positions
                    .get(&CHAR_B_ID)
                    .map(|p| [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]),
            ),
        )
    };

    let tourney_match_involves_b = |m: &eve_db::service_domains::TourneyMatch| {
        m.red_team_id == CORP_B_ID
            || m.red_team_id == ALLIANCE_B_ID
            || m.blue_team_id == CORP_B_ID
            || m.blue_team_id == ALLIANCE_B_ID
            || m.red_fleet_boss_id == Some(CHAR_B_ID)
            || m.blue_fleet_boss_id == Some(CHAR_B_ID)
            || m.red_fleet_id == Some(b_fleet_id)
            || m.blue_fleet_id == Some(b_fleet_id)
            || m.red_players
                .iter()
                .any(|p| p.character_id == CHAR_B_ID || b_item_ids.contains(&p.ship_id))
            || m.blue_players
                .iter()
                .any(|p| p.character_id == CHAR_B_ID || b_item_ids.contains(&p.ship_id))
            || m.referees.contains(&CHAR_B_ID)
            || m.pilot_errors.contains_key(&CHAR_B_ID)
            || m.teleported_teams.contains(&CORP_B_ID)
            || m.teleported_teams.contains(&ALLIANCE_B_ID)
            || m.returned_teams.contains(&CORP_B_ID)
            || m.returned_teams.contains(&ALLIANCE_B_ID)
            || m.beacon_ids.iter().any(|id| b_item_ids.contains(id))
    };
    let b_tourney_match_ids: BTreeSet<i64> = {
        let mut match_ids = BTreeSet::new();
        let mut collect = |tr: &eve_db::service_domains::TourneyStore| {
            for (&mid, m) in &tr.matches {
                if tourney_match_involves_b(m) {
                    match_ids.insert(mid);
                }
            }
        };
        collect(before.tourney);
        collect(after.tourney);
        match_ids
    };
    let tourney_b = |tr: &eve_db::service_domains::TourneyStore| {
        let eve_db::service_domains::TourneyStore {
            matches,
            active_match_id,
            ban_sessions,
            tournaments,
            arena_systems,
        } = tr;
        (
            matches
                .iter()
                .filter(|(&mid, m)| {
                    b_tourney_match_ids.contains(&mid) || tourney_match_involves_b(m)
                })
                .map(|(&mid, m)| (mid, m.clone()))
                .collect::<BTreeMap<_, _>>(),
            (*active_match_id).filter(|mid| b_tourney_match_ids.contains(mid)),
            ban_sessions
                .iter()
                .filter(|(&k, v)| {
                    (k >= 1_000_000 && b_tourney_match_ids.contains(&(k / 1_000_000)))
                        || v.contains(&CHAR_B_ID)
                })
                .map(|(&k, v)| (k, v.clone()))
                .collect::<BTreeMap<_, _>>(),
            tournaments
                .iter()
                .filter(|(_, s)| text_mentions_any_id(s, b_all_ids))
                .map(|(&k, s)| (k, s.clone()))
                .collect::<BTreeMap<_, _>>(),
            arena_systems
                .iter()
                .filter(|(_, sys)| {
                    sys.combat_beacons.iter().any(|id| b_item_ids.contains(id))
                        || sys.warp_bubbles.iter().any(|id| b_item_ids.contains(id))
                })
                .map(|(&k, sys)| (k, sys.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    };

    let b_dep_ids: BTreeSet<i64> = {
        let mut ids = BTreeSet::new();
        let mut collect = |dep: &eve_db::service_domains::MobileDeployableRuntimeState| {
            for (&id, d) in &dep.deployables {
                if b_item_ids.contains(&id)
                    || b_entity_ids.contains(&d.owner_id)
                    || d.deployed_fleet_id == Some(b_fleet_id)
                {
                    ids.insert(id);
                }
            }
        };
        collect(before.deployable);
        collect(after.deployable);
        ids
    };
    let deployables_b = |dep: &eve_db::service_domains::MobileDeployableRuntimeState| {
        let eve_db::service_domains::MobileDeployableRuntimeState {
            deployables,
            damaged_items,
            observatory_pings_done,
            cloak_activated_unix,
            mmju_spools,
            siphon_last_cycle_unix,
            pending_decloak_notifications,
        } = dep;
        let is_b_dep_or_item = |id: i64| b_item_ids.contains(&id) || b_dep_ids.contains(&id);
        (
            deployables
                .iter()
                .filter(|(&id, d)| {
                    b_dep_ids.contains(&id)
                        || b_entity_ids.contains(&d.owner_id)
                        || d.deployed_fleet_id == Some(b_fleet_id)
                })
                .map(|(&id, d)| (id, d.clone()))
                .collect::<BTreeMap<_, _>>(),
            damaged_items
                .iter()
                .filter(|(&id, _)| is_b_dep_or_item(id))
                .map(|(&id, d)| {
                    (
                        id,
                        (
                            d.shield_hp.to_bits(),
                            d.max_shield_hp.to_bits(),
                            d.armor_hp.to_bits(),
                            d.max_armor_hp.to_bits(),
                            d.structure_hp.to_bits(),
                            d.max_structure_hp.to_bits(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            observatory_pings_done
                .iter()
                .filter(|(&id, _)| is_b_dep_or_item(id))
                .map(|(&id, &n)| (id, n))
                .collect::<BTreeMap<_, _>>(),
            cloak_activated_unix
                .iter()
                .filter(|(&id, _)| is_b_dep_or_item(id))
                .map(|(&id, &ts)| (id, ts))
                .collect::<BTreeMap<_, _>>(),
            mmju_spools
                .iter()
                .filter(|(&k, sp)| {
                    is_b_dep_or_item(k)
                        || sp.char_id == CHAR_B_ID
                        || is_b_dep_or_item(sp.ship_id)
                        || is_b_dep_or_item(sp.unit_id)
                })
                .map(|(&k, sp)| {
                    (
                        k,
                        (
                            sp.ship_id,
                            sp.char_id,
                            sp.unit_id,
                            sp.solar_system_id,
                            sp.started_unix,
                            sp.complete_at_unix,
                            [
                                sp.direction[0].to_bits(),
                                sp.direction[1].to_bits(),
                                sp.direction[2].to_bits(),
                            ],
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            siphon_last_cycle_unix
                .iter()
                .filter(|(&id, _)| is_b_dep_or_item(id))
                .map(|(&id, &ts)| (id, ts))
                .collect::<BTreeMap<_, _>>(),
            pending_decloak_notifications
                .iter()
                .filter(|n| {
                    n.char_id == CHAR_B_ID
                        || is_b_dep_or_item(n.ship_id)
                        || is_b_dep_or_item(n.module_id)
                })
                .cloned()
                .collect::<Vec<_>>(),
        )
    };

    if pvp_b(before.pvp) != pvp_b(after.pvp)
        || tourney_b(before.tourney) != tourney_b(after.tourney)
        || deployables_b(before.deployable) != deployables_b(after.deployable)
    {
        return true;
    }

    // Node graphs, seasons/campaigns, quasar commerce, fleet_and_social_ops, and cluster_and_runtime.
    let node_b = |ng: &eve_db::service_domains::NodeGraphAndProgressionStore| {
        (
            ng.air_npe_runners.get(&CHAR_B_ID).cloned(),
            ng.client_node_graphs.get(&CHAR_B_ID).cloned(),
            ng.node_graph_blackboards
                .iter()
                .filter(|(&(cid, _), _)| cid == CHAR_B_ID)
                .map(|(&k, v)| (k, v.clone()))
                .collect::<BTreeMap<_, _>>(),
            ng.expert_systems.get(&CHAR_B_ID).cloned(),
            ng.certificates.get(&CHAR_B_ID).cloned(),
        )
    };
    let seasons_b = |sc: &eve_db::service_domains::SeasonsAndCampaignsStore| {
        (
            sc.season_states.get(&CHAR_B_ID).cloned(),
            sc.active_seasons.get(&CHAR_B_ID).copied(),
            sc.hacking_wins.get(&CHAR_B_ID).copied(),
            sc.milestone_playtime.get(&CHAR_B_ID).cloned(),
            sc.login_campaign_enrollments.get(&CHAR_B_ID).copied(),
            sc.auto_injected_rewards.get(&CHAR_B_ID).cloned(),
            sc.login_reward_expiry.get(&CHAR_B_ID).copied(),
            sc.project_discovery_players.get(&CHAR_B_ID).cloned(),
            sc.character_activities.get(&CHAR_B_ID).cloned(),
            sc.fast_checkout_states.get(&USER_B_ID).cloned(),
        )
    };
    let quasar_b = |qc: &eve_db::service_domains::QuasarCommerceStore| {
        let eve_db::service_domains::QuasarCommerceStore {
            payment_tokens,
            payment_receipts,
            payment_schedules,
            plex_tx_logs,
            nes_store_catalog: _,
            nes_store_offer_tags: _,
            plex_invoices: _,
            cosmetics_market,
            orchestrator: _,
            failed_fulfillments,
            targeted_offers,
            acl_registry,
            lp_asset_ops_completed: _,
            store_purchase_idempotency,
        } = qc;
        let char_b_u32 = u32::try_from(CHAR_B_ID).ok();
        (
            payment_tokens
                .iter()
                .filter(|r| r.user_id == USER_B_ID)
                .cloned()
                .collect::<Vec<_>>(),
            payment_receipts
                .iter()
                .filter(|r| r.user_id == USER_B_ID)
                .cloned()
                .collect::<Vec<_>>(),
            payment_schedules
                .iter()
                .filter(|r| r.user_id == USER_B_ID)
                .cloned()
                .collect::<Vec<_>>(),
            plex_tx_logs
                .iter()
                .filter(|r| r.user_id == USER_B_ID)
                .cloned()
                .collect::<Vec<_>>(),
            cosmetics_market
                .as_ref()
                .map(|cm| {
                    let eve_db::service_domains::CosmeticsMarketData {
                        component_listings: _,
                        skin_listings,
                        transactions,
                        next_seq: _,
                    } = cm;
                    let listings = skin_listings
                        .iter()
                        .filter(|(_, l)| {
                            Some(l.seller_id) == char_b_u32
                                || match l.target {
                                    eve_db::service_domains::ListingTargetRecord::Public => false,
                                    eve_db::service_domains::ListingTargetRecord::Corporation(id) => {
                                        i64::try_from(id).ok() == Some(CORP_B_ID)
                                    }
                                    eve_db::service_domains::ListingTargetRecord::Alliance(id) => {
                                        i64::try_from(id).ok() == Some(ALLIANCE_B_ID)
                                    }
                                    eve_db::service_domains::ListingTargetRecord::Character(id) => {
                                        i64::try_from(id).ok() == Some(CHAR_B_ID)
                                    }
                                }
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>();
                    let txs = transactions
                        .iter()
                        .filter(|(_, tx)| {
                            char_b_u32.is_some()
                                && (tx.buyer_id == char_b_u32 || tx.seller_id == char_b_u32)
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>();
                    (listings, txs)
                })
                .unwrap_or_default(),
            failed_fulfillments
                .iter()
                .filter(|r| r.user_id == USER_B_ID)
                .cloned()
                .collect::<Vec<_>>(),
            targeted_offers
                .iter()
                .filter(|(k, r)| {
                    r.user_id == USER_B_ID || text_mentions_any_id(k, &[USER_B_ID as i64])
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            acl_registry
                .as_ref()
                .map(|acls| {
                    acls.iter()
                        .filter(|r| {
                            r.characters
                                .iter()
                                .any(|&(id, _)| i64::try_from(id).ok() == Some(CHAR_B_ID))
                                || r.corporations
                                    .iter()
                                    .any(|&(id, _)| i64::try_from(id).ok() == Some(CORP_B_ID))
                                || r.alliances
                                    .iter()
                                    .any(|&(id, _)| i64::try_from(id).ok() == Some(ALLIANCE_B_ID))
                                || text_mentions_any_id(&r.name, b_all_ids)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            store_purchase_idempotency
                .iter()
                .filter(|(_, r)| r.buyer_user_id == USER_B_ID)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let fleet_ops_b = |fo: &eve_db::service_domains::FleetAndSocialOpsStore| {
        (
            (
                fo.fleet_saved_setups.get(&CHAR_B_ID).map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), serde_json::to_value(v).ok()))
                        .collect::<BTreeMap<_, _>>()
                }),
                [CHAR_B_ID, b_fleet_id]
                    .into_iter()
                    .filter_map(|id| {
                        fo.fleet_stored_formations
                            .get(&id)
                            .map(|&(l, x, y)| (id, (l, x.to_bits(), y.to_bits())))
                    })
                    .collect::<BTreeMap<_, _>>(),
                fo.fleet_adverts
                    .get(&b_fleet_id)
                    .and_then(|v| serde_json::to_value(v).ok()),
                fo.fleet_join_requests
                    .iter()
                    .filter(|(&fid, set)| fid == b_fleet_id || set.contains(&CHAR_B_ID))
                    .map(|(&k, v)| (k, v.iter().copied().collect::<BTreeSet<_>>()))
                    .collect::<BTreeMap<_, _>>(),
                fo.fleet_watchlist
                    .iter()
                    .filter(|(&cid, set)| {
                        cid == CHAR_B_ID || cid == b_fleet_id || set.contains(&CHAR_B_ID)
                    })
                    .map(|(&k, v)| (k, v.iter().copied().collect::<BTreeSet<_>>()))
                    .collect::<BTreeMap<_, _>>(),
                fo.fleet_voice_enabled.get(&b_fleet_id).copied(),
                fo.fleet_loot_logging.get(&b_fleet_id).copied(),
                fo.corp_recruitment_ads
                    .iter()
                    .filter(|ad| ad.corp_id == CORP_B_ID || ad.recruiter_id == CHAR_B_ID)
                    .filter_map(|ad| serde_json::to_value(ad).ok())
                    .collect::<Vec<_>>(),
                fo.corp_allow_war.get(&CORP_B_ID).copied(),
            ),
            (
                fo.ingamereport_tickets
                    .iter()
                    .filter(|t| {
                        t.reporter_char_id == CHAR_B_ID
                            || t.reporter_corp_id == CORP_B_ID
                            || t.reported_entity_id == CHAR_B_ID
                            || t.reported_entity_id == CORP_B_ID
                            || t.report_id == b_petition_id
                    })
                    .filter_map(|t| serde_json::to_value(t).ok())
                    .collect::<Vec<_>>(),
                fo.contract_deposits.get(&b_contract_id).copied(),
                fo.delayed_rewards.get(&CHAR_B_ID).map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                }),
                fo.agent_char_disabled_missions
                    .get(&CHAR_B_ID)
                    .map(|s| s.iter().copied().collect::<BTreeSet<_>>()),
                fo.wh_local_witnesses
                    .iter()
                    .filter(|(_, set)| set.contains(&CHAR_B_ID))
                    .map(|(k, _)| k.clone())
                    .collect::<BTreeSet<_>>(),
                fo.hypernet_subs
                    .iter()
                    .filter(|(_, ids)| ids.contains(&CHAR_B_ID))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>(),
                fo.agent_replay_ts
                    .iter()
                    .filter(|(&(cid, _), _)| cid == CHAR_B_ID)
                    .map(|(&k, &v)| (k, v))
                    .collect::<BTreeMap<_, _>>(),
                fo.signal_tracker_subs
                    .iter()
                    .filter(|(_, ids)| ids.contains(&CHAR_B_ID))
                    .map(|(&k, v)| (k, v.clone()))
                    .collect::<BTreeMap<_, _>>(),
                fo.pending_probe_scans
                    .iter()
                    .filter(|(cid, _, _, _, _, _)| *cid == CHAR_B_ID)
                    .count(),
            ),
        )
    };
    let cluster_b = |cl: &eve_db::service_domains::ClusterAndRuntimeStore| {
        let eve_db::service_domains::ClusterAndRuntimeStore {
            cluster_misc_state,
            clone_vat_bay_state,
            structure_defense,
            alliance_sov_cfg,
            load_service_state: _,
            active_sessions,
            session_registry,
            cluster_node_table: _,
            cluster_admin_broadcasts: _,
            cluster_service_states: _,
            cluster_attacks: _,
            external_queue_events: _,
            cluster_downtime_state: _,
            macho_client_code_hash: _,
            macho_global_config: _,
            user_sessions,
            gm_audit_log: _,
            alert_bean_counts: _,
            alert_log_modes: _,
            alert_client_traces: _,
            alert_proxy_traces: _,
            alert_group_bean_bytes: _,
            browser_lockdown_policy: _,
            localization_qa_wrap_users,
            client_stat_logger_strings: _,
            blacklisted_planets: _,
            portraits,
            paperdoll_states,
            char_system_visits,
            module_reload_until_unix,
            auto_reloaded_event,
            warp_vector_cooldown_until,
            ship_clone_offers,
            mct_slots,
            account_resculpt_tokens,
            resculpt_pending,
            char_cohorts,
            user_cohorts,
            character_locks,
            omega_downgraded,
            server_runtime_json,
        } = cl;
        let eve_db::service_domains::CloneVatBayState { active_by_ship } = clone_vat_bay_state;
        let misc_b = cluster_misc_state
            .as_ref()
            .map(|misc| {
                let eve_db::service_domains::PersistedClusterMiscState {
                    persisted_shipcaster_state: _,
                    char_settings,
                    landing_pads,
                    launchers,
                    starter_launchers,
                    used_starter_shipcaster_chars,
                    offered_load_slots,
                    char_energy,
                    target_cooldowns,
                    target_charges,
                    zarzakh_toll_quotes,
                    zarzakh_horizon_siege: _,
                } = misc;
                (
                    char_settings.get(&CHAR_B_ID).cloned(),
                    used_starter_shipcaster_chars.contains(&CHAR_B_ID),
                    offered_load_slots.get(&CHAR_B_ID).cloned(),
                    char_energy.get(&CHAR_B_ID).cloned(),
                    target_cooldowns
                        .iter()
                        .filter(|(&pid, _)| b_item_ids.contains(&pid))
                        .map(|(&k, &v)| (k, v))
                        .collect::<BTreeMap<_, _>>(),
                    target_charges
                        .iter()
                        .filter(|(&pid, _)| b_item_ids.contains(&pid))
                        .map(|(&k, &v)| (k, v))
                        .collect::<BTreeMap<_, _>>(),
                    zarzakh_toll_quotes.get(&CHAR_B_ID).cloned(),
                    landing_pads
                        .iter()
                        .filter(|p| b_item_ids.contains(&p.item_id))
                        .map(|p| (p.item_id, p.clone()))
                        .collect::<BTreeMap<_, _>>(),
                    launchers
                        .iter()
                        .filter(|l| b_item_ids.contains(&l.item_id))
                        .map(|l| (l.item_id, l.clone()))
                        .collect::<BTreeMap<_, _>>(),
                    starter_launchers
                        .iter()
                        .filter(|s| b_item_ids.contains(&s.item_id))
                        .map(|s| (s.item_id, s.clone()))
                        .collect::<BTreeMap<_, _>>(),
                )
            })
            .unwrap_or_default();
        (
            (
                b_struct_ids
                    .iter()
                    .filter_map(|&sid| structure_defense.get(&sid).map(|v| (sid, v.clone())))
                    .collect::<BTreeMap<_, _>>(),
                alliance_sov_cfg.get(&ALLIANCE_B_ID).cloned(),
                active_sessions
                    .iter()
                    .copied()
                    .filter(|id| b_entity_ids.contains(id))
                    .collect::<Vec<_>>(),
                session_registry
                    .iter()
                    .filter(|(k, v)| {
                        text_mentions_any_id(k, b_entity_ids)
                            || json_mentions_any_id(v, b_entity_ids)
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>(),
                portraits
                    .iter()
                    .filter(|(k, _)| text_mentions_any_id(k, b_entity_ids))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>(),
                paperdoll_states.get(&CHAR_B_ID).cloned(),
            ),
            (
                char_system_visits.get(&CHAR_B_ID).cloned(),
                b_item_ids
                    .iter()
                    .filter_map(|&id| module_reload_until_unix.get(&id).map(|&u| (id, u)))
                    .collect::<BTreeMap<_, _>>(),
                b_item_ids
                    .iter()
                    .filter_map(|&id| ship_clone_offers.get(&id).map(|&v| (id, v)))
                    .collect::<BTreeMap<_, _>>(),
                warp_vector_cooldown_until.get(&CHAR_B_ID).copied(),
                mct_slots.get(&USER_B_ID).cloned(),
                account_resculpt_tokens
                    .iter()
                    .filter(|(k, _)| text_mentions_any_id(k, b_entity_ids))
                    .map(|(k, &v)| (k.clone(), v))
                    .collect::<BTreeMap<_, _>>(),
            ),
            (
                misc_b,
                active_by_ship
                    .iter()
                    .filter(|(&sid, &(mid, _))| {
                        b_item_ids.contains(&sid) || b_item_ids.contains(&mid)
                    })
                    .map(|(&k, &v)| (k, v))
                    .collect::<BTreeMap<_, _>>(),
                auto_reloaded_event
                    .iter()
                    .filter(|(&mid, (linked, _, _))| {
                        b_item_ids.contains(&mid) || linked.iter().any(|tid| b_item_ids.contains(tid))
                    })
                    .map(|(&mid, (linked, ammo, ms))| (mid, (linked.clone(), *ammo, ms.to_bits())))
                    .collect::<BTreeMap<_, _>>(),
                resculpt_pending.contains(&CHAR_B_ID),
                char_cohorts.get(&CHAR_B_ID).cloned(),
                user_cohorts.get(&USER_B_ID).cloned(),
            ),
            (
                character_locks.get(&CHAR_B_ID).cloned(),
                omega_downgraded.contains(&USER_B_ID),
                [USER_B_ID as i64, CHAR_B_ID]
                    .into_iter()
                    .filter_map(|id| {
                        localization_qa_wrap_users.get(&id).map(|&v| (id, v))
                    })
                    .collect::<BTreeMap<_, _>>(),
                user_sessions
                    .iter()
                    .filter(|v| {
                        v.get("user_id").and_then(Value::as_u64) == Some(USER_B_ID)
                            || v.get("character_id").and_then(Value::as_i64) == Some(CHAR_B_ID)
                            || json_mentions_any_id(v, b_all_ids)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                note_or_json_b_slice(server_runtime_json.as_ref(), b_all_ids),
            ),
        )
    };
    node_b(before.node_graph) != node_b(after.node_graph)
        || seasons_b(before.seasons) != seasons_b(after.seasons)
        || quasar_b(before.quasar) != quasar_b(after.quasar)
        || fleet_ops_b(before.fleet_ops) != fleet_ops_b(after.fleet_ops)
        || cluster_b(before.cluster) != cluster_b(after.cluster)
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_negative_call(
    fix: &MatrixFixture,
    runtime_baseline: &RuntimeBaseline,
    sit: Situation,
    sit_label: &str,
    svc: &str,
    bound_id: Option<u64>,
    meth: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
    allow_rule: Option<&AllowRule>,
    observed: &mut BTreeSet<MatrixViolation>,
) {
    let is_allowlisted = allow_rule.is_some();
    let allow_caller_scoped_mutation =
        sit == Situation::B && allow_rule.is_some_and(|r| r.caller_scoped_mutation);

    let epoch_before = fix.ctx.db.state_epoch();
    let commits_before = fix.ctx.db.tx_commit_count();
    let mut call_sess = fix.sess_by_sit[sit as usize].clone();
    let out = {
        let mut h = common::borrow_wire_harness(&fix.ctx, &mut call_sess)
            .with_mode(ContractValidationMode::ReportOnly);
        h.call_exchange(svc, bound_id, meth, args, kw).await
    };

    let db_counter_bumped = fix.ctx.db.state_epoch() != epoch_before
        || fix.ctx.db.tx_commit_count() != commits_before;
    let cur_db = if db_counter_bumped {
        Some(fix.ctx.db.snapshot_tables())
    } else {
        None
    };
    let db_dirty = cur_db
        .as_ref()
        .is_some_and(|tables| tables.snapshot != runtime_baseline.db.snapshot);
    let (runtime_dirty, bound_dirty, space_dirty) =
        check_runtime_changed(fix, runtime_baseline);
    let world_mutated = db_dirty || runtime_dirty;

    if is_allowlisted {
        if let Err(err) = &out.result {
            if matches!(sit, Situation::E | Situation::F) {
                assert!(
                    !matches!(
                        err.code.as_str(),
                        "MustBeDocked"
                            | "NotDocked"
                            | "NotInStation"
                            | "CharacterMustBeDocked"
                            | "MustBeDockedAtTargetStation"
                            | "CannotUseWhileInSpace"
                    ),
                    "stale/inaccurate allowlist in Situation {sit_label} for {svc}.{meth}: returned prerequisite error {}: {}",
                    err.code,
                    err.message
                );
            } else if matches!(sit, Situation::C | Situation::D) {
                assert!(
                    !matches!(
                        err.code.as_str(),
                        "CannotSelfDestructDocked"
                            | "CannotSelfDestructWhileDocked"
                            | "MustBeInSpace"
                            | "MustBeUndocked"
                            | "NotInSpace"
                            | "ShipDocked"
                            | "CannotEjectWhileDocked"
                            | "CantDoThatWhileDocked"
                            | "CannotLinkWeaponsWhileDocked"
                            | "CannotRepairModuleWhileDocked"
                            | "CannotRepairNaniteWhileDocked"
                            | "CantLaunchDronesWhileDocked"
                    ),
                    "stale/inaccurate allowlist in Situation {sit_label} for {svc}.{meth}: returned prerequisite error {}: {}",
                    err.code,
                    err.message
                );
            }
        }
    }
    if !is_allowlisted && out.result.is_ok() {
        observed.insert(MatrixViolation {
            situation: sit_label.to_string(),
            service: svc.to_string(),
            method: meth.to_string(),
            kind: "unexpected_allow".to_string(),
        });
    }
    let mutation_violation = if !world_mutated {
        false
    } else if !is_allowlisted || sit == Situation::A {
        true
    } else if sit == Situation::B {
        if allow_caller_scoped_mutation && out.result.is_ok() {
            // Caller-scoped mutations allowlisted in Situation B may mutate caller (CHAR_A_ID)
            // state, but must still preserve strict target (CHAR_B_ID / CORP_B_ID) invariance.
            target_b_state_mutated(
                fix,
                runtime_baseline,
                cur_db.as_ref(),
                bound_dirty,
                space_dirty,
            )
        } else {
            // Read-only Situation B allowlist entries and rejected calls require full invariance.
            true
        }
    } else {
        false
    };
    if mutation_violation {
        observed.insert(MatrixViolation {
            situation: sit_label.to_string(),
            service: svc.to_string(),
            method: meth.to_string(),
            kind: "world_mutated".to_string(),
        });
    }

    if db_counter_bumped || runtime_dirty {
        restore_world_state(fix, runtime_baseline, db_dirty, bound_dirty, space_dirty).await;
    }
}

async fn assert_regression_1182_1183_guards(
    fix: &MatrixFixture,
    runtime_baseline: &RuntimeBaseline,
    kw: &HashMap<String, PyRep>,
) {
    // #1182: non-GM setting tax on NPC station (b), Corp B structure (b), or roleless Corp A structure (c)
    for (label, sit, facility_id) in [
        ("#1182 NPC station tax by Char A", Situation::B, NPC_STATION_ID),
        ("#1182 Corp B structure tax by Char A", Situation::B, CORP_B_STRUCTURE_ID),
        ("#1182 Corp A structure tax by roleless Char C", Situation::C, CORP_A_STRUCTURE_ID),
    ] {
        let mut s = fix.sess_by_sit[sit as usize].clone();
        let out = {
            let mut h = common::borrow_wire_harness(&fix.ctx, &mut s)
                .with_mode(ContractValidationMode::ReportOnly);
            h.call_exchange(
                "facilityManager",
                None,
                "SetFacilityTaxes",
                &[
                    PyRep::Integer(facility_id),
                    PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.05))]),
                ],
                kw,
            )
            .await
        };
        assert!(out.result.is_err(), "{label} must reject via wire");
        assert!(
            fix.ctx.db.snapshot_tables().snapshot == runtime_baseline.db.snapshot,
            "{label} must leave DB unchanged"
        );
    }

    // #1183: Reprocess outputting into Character B's container (b) or roleless Corp A office hangar (c)
    for (label, sit, src_ore, out_owner, out_loc, out_flag) in [
        ("#1183 Reprocess into Char B container", Situation::B, fix.a_ore_id, CHAR_B_ID, fix.b_cont_id, 4i64),
        ("#1183 Reprocess into roleless Corp A office", Situation::C, fix.c_ore_id, CORP_A_ID, CORP_A_OFFICE_ID, 117i64),
    ] {
        let mut s = fix.sess_by_sit[sit as usize].clone();
        let out = {
            let mut h = common::borrow_wire_harness(&fix.ctx, &mut s)
                .with_mode(ContractValidationMode::ReportOnly);
            h.call_exchange(
                "reprocessingSvc",
                None,
                "Reprocess",
                &[
                    PyRep::List(vec![PyRep::Integer(src_ore)]),
                    PyRep::Integer(NPC_STATION_ID),
                    PyRep::Integer(out_owner),
                    PyRep::Integer(out_loc),
                    PyRep::Integer(out_flag),
                ],
                kw,
            )
            .await
        };
        assert!(out.result.is_err(), "{label} must reject via wire");
        assert!(
            fix.ctx.db.snapshot_tables().snapshot == runtime_baseline.db.snapshot,
            "{label} must leave DB unchanged"
        );
    }
}

async fn run_situation_worker(
    sde: Arc<SdeRegistry>,
    discovered: Arc<BTreeSet<(String, String)>>,
    allowlist: Arc<AllowlistMap>,
    svc_methods: Arc<HashMap<String, Vec<String>>>,
    sit: Situation,
) -> BTreeSet<MatrixViolation> {
    let fix = setup_fixture(sde).await;
    let runtime_baseline = capture_runtime_baseline(&fix);
    let kw = HashMap::new();
    let sit_code = sit.code();
    let mut observed = BTreeSet::new();

    for (svc, meth) in discovered.iter() {
        let allow_rule = allowlist
            .get(&(svc.clone(), meth.clone()))
            .and_then(|m| m.get(sit_code));
        let (bound_id, args) = build_call_args(&fix, sit, svc, meth);
        evaluate_negative_call(
            &fix,
            &runtime_baseline,
            sit,
            sit_code,
            svc,
            bound_id,
            meth,
            &args,
            &kw,
            allow_rule,
            &mut observed,
        )
        .await;
    }

    if sit == Situation::B {
        for (svc, b_bound_id) in &fix.b_bounds {
            if let Some(methods) = svc_methods.get(svc) {
                for meth in methods {
                    let (_, args) = build_call_args(&fix, Situation::B, svc, meth);
                    evaluate_negative_call(
                        &fix,
                        &runtime_baseline,
                        Situation::B,
                        "b_bound",
                        svc,
                        Some(*b_bound_id),
                        meth,
                        &args,
                        &kw,
                        None,
                        &mut observed,
                    )
                    .await;
                }
            }
        }
        assert_regression_1182_1183_guards(&fix, &runtime_baseline, &kw).await;
    }

    observed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_negative_test_matrix_and_ratchet() {
    let t_start = std::time::Instant::now();
    let root = workspace_root();
    let (discovered_raw, _bindable) = discover_registered_rpcs(&root);
    let allowlist_raw = load_allowlist_and_verify_parity(&root, &discovered_raw);
    let update_baseline = std::env::var("EVOMU_UPDATE_NEG_MATRIX_BASELINE").as_deref() == Ok("1");
    let baseline_ratchet = if update_baseline {
        BTreeSet::new()
    } else {
        load_baseline_violations(&root)
    };

    let mut svc_methods_raw: HashMap<String, Vec<String>> = HashMap::new();
    for (svc, meth) in &discovered_raw {
        if meth != "MachoBindObject" && meth != "MachoResolveObject" {
            svc_methods_raw
                .entry(svc.clone())
                .or_default()
                .push(meth.clone());
        }
    }

    let sde = Arc::new(
        SdeRegistry::open(&SdeRegistry::default_db_path())
            .expect("open SDE SQLite database"),
    );
    let discovered = Arc::new(discovered_raw);
    let allowlist = Arc::new(allowlist_raw);
    let svc_methods = Arc::new(svc_methods_raw);

    let mut join_set = tokio::task::JoinSet::new();
    for sit in Situation::ALL {
        let sde = Arc::clone(&sde);
        let discovered = Arc::clone(&discovered);
        let allowlist = Arc::clone(&allowlist);
        let svc_methods = Arc::clone(&svc_methods);
        join_set.spawn(async move {
            run_situation_worker(sde, discovered, allowlist, svc_methods, sit).await
        });
    }

    let mut observed_violations: BTreeSet<MatrixViolation> = BTreeSet::new();
    while let Some(res) = join_set.join_next().await {
        let part = res.expect("situation matrix worker must not panic");
        observed_violations.extend(part);
    }

    eprintln!(
        "Negative matrix finished in {:.2}s ({} RPCs x 6 situations + b_bound; {} violations)",
        t_start.elapsed().as_secs_f64(),
        discovered.len(),
        observed_violations.len()
    );

    // Optional baseline regeneration mode during explicit ratchet seeding
    if update_baseline {
        write_baseline_violations(&root, &observed_violations);
        eprintln!(
            "Updated {} with {} baseline violations",
            violations_path(&root).display(),
            observed_violations.len()
        );
        return;
    }

    // Enforce no-regression ratchet: reject newly unratcheted violations and log resolved baseline entries
    let new_violations: Vec<&MatrixViolation> =
        observed_violations.difference(&baseline_ratchet).collect();
    assert!(
        new_violations.is_empty(),
        "Negative test matrix discovered {} new unratcheted violation(s) out of {} RPCs:\n{:#?}",
        new_violations.len(),
        discovered.len(),
        &new_violations[..new_violations.len().min(40)]
    );

    let fixed_violations: Vec<&MatrixViolation> =
        baseline_ratchet.difference(&observed_violations).collect();
    let fixed_count = fixed_violations.len();
    if fixed_count > 0 {
        eprintln!(
            "INFO: negative test matrix resolved {fixed_count} baseline violation(s) \
             (remaining {} vs committed {})",
            observed_violations.len(),
            baseline_ratchet.len()
        );
    }
}

#[test]
fn target_b_subfield_detectors_catch_target_b_and_ignore_actor_a() {
    use eve_db::service_domains::{
        CharacterEnergyState, ClusterAndRuntimeStore, CosmeticTransactionRecord, CosmeticsMarketData,
        FailedFulfillmentRecord, FleetAndSocialOpsStore, ListingTargetRecord, MmjuSpoolEntry,
        MobileDeployableRuntimeState, NodeGraphAndProgressionStore, OfferedLoadSlot,
        PendingDecloakNotice, PendingZarzakhTollQuote, PersistedClusterMiscState,
        PersistedDamageRecord, PvpArenaMatchInstance, PvpFilamentStore, QuasarAclRecord,
        QuasarCommerceStore, SeasonsAndCampaignsStore, SkinListingRecord, TargetedOfferAdminRecord,
        TourneyMatch, TourneyPilot, TourneyStore,
    };

    #[derive(Clone, Default)]
    struct OwnedStores {
        pvp: PvpFilamentStore,
        tourney: TourneyStore,
        deployable: MobileDeployableRuntimeState,
        node_graph: NodeGraphAndProgressionStore,
        seasons: SeasonsAndCampaignsStore,
        quasar: QuasarCommerceStore,
        fleet_ops: FleetAndSocialOpsStore,
        cluster: ClusterAndRuntimeStore,
    }
    impl OwnedStores {
        fn as_refs(&self) -> TargetBStoreRefs<'_> {
            TargetBStoreRefs {
                pvp: &self.pvp,
                tourney: &self.tourney,
                deployable: &self.deployable,
                node_graph: &self.node_graph,
                seasons: &self.seasons,
                quasar: &self.quasar,
                fleet_ops: &self.fleet_ops,
                cluster: &self.cluster,
            }
        }
    }

    let b_fleet_id = 777_002;
    let b_contract_id = 888_002;
    let b_petition_id = 999_002;
    let b_ship_id = 1_500_002;
    let b_mod_id = 1_500_012;
    let a_ship_id = 1_500_001;
    let a_mod_id = 1_500_011;

    let b_entity_ids = vec![
        CHAR_B_ID,
        USER_B_ID as i64,
        CORP_B_ID,
        ALLIANCE_B_ID,
        b_ship_id,
        b_fleet_id,
        b_contract_id,
        b_petition_id,
        CORP_B_STRUCTURE_ID,
    ];
    let b_struct_ids: BTreeSet<i64> = [CORP_B_STRUCTURE_ID].into_iter().collect();
    let b_item_ids: BTreeSet<i64> = b_entity_ids
        .iter()
        .copied()
        .chain([b_mod_id])
        .collect();
    let b_all_ids: Vec<i64> = b_item_ids
        .iter()
        .copied()
        .filter(|&id| id >= 100_000)
        .collect();
    let scope = TargetBDomainScope {
        b_fleet_id,
        b_contract_id,
        b_petition_id,
        b_entity_ids: &b_entity_ids,
        b_item_ids: &b_item_ids,
        b_struct_ids: &b_struct_ids,
        b_all_ids: &b_all_ids,
    };

    let check_case = |base: &OwnedStores, mutate: &dyn Fn(&mut OwnedStores)| -> bool {
        let mut after = base.clone();
        mutate(&mut after);
        target_b_sub_stores_mutated(&scope, &base.as_refs(), &after.as_refs())
    };
    let assert_b = |label: &str, base: &OwnedStores, mutate: &dyn Fn(&mut OwnedStores)| {
        assert!(check_case(base, mutate), "Target B mutation missed: {label}");
    };
    let assert_a = |label: &str, base: &OwnedStores, mutate: &dyn Fn(&mut OwnedStores)| {
        assert!(!check_case(base, mutate), "Actor A/global false positive: {label}");
    };

    let mut base = OwnedStores::default();
    base.tourney.matches.insert(
        21,
        TourneyMatch {
            match_id: 21,
            red_players: vec![TourneyPilot {
                character_id: CHAR_B_ID,
                ship_id: b_ship_id,
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    base.tourney.matches.insert(
        22,
        TourneyMatch {
            match_id: 22,
            red_players: vec![TourneyPilot {
                character_id: CHAR_A_ID,
                ship_id: a_ship_id,
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    base.cluster.server_runtime_json = Some(
        serde_json::json!({
            "actors": [
                {"character_id": CHAR_A_ID, "counter": 1},
                {"character_id": CHAR_B_ID, "counter": 1}
            ]
        })
        .to_string(),
    );

    // 1. fleet_ops: char_id keying on fleet_stored_formations & fleet_watchlist
    assert_b("fleet_stored_formations by CHAR_B_ID", &base, &|s| {
        s.fleet_ops.fleet_stored_formations.insert(CHAR_B_ID, (1, 10.0, 20.0));
    });
    assert_a("fleet_stored_formations by CHAR_A_ID", &base, &|s| {
        s.fleet_ops.fleet_stored_formations.insert(CHAR_A_ID, (1, 10.0, 20.0));
    });
    assert_b("fleet_watchlist watcher CHAR_B_ID", &base, &|s| {
        s.fleet_ops
            .fleet_watchlist
            .insert(CHAR_B_ID, [CHAR_A_ID].into_iter().collect());
    });
    assert_b("fleet_watchlist watched CHAR_B_ID", &base, &|s| {
        s.fleet_ops
            .fleet_watchlist
            .insert(CHAR_A_ID, [CHAR_B_ID].into_iter().collect());
    });
    assert_a("fleet_watchlist Actor A/C", &base, &|s| {
        s.fleet_ops
            .fleet_watchlist
            .insert(CHAR_A_ID, [CHAR_C_ID].into_iter().collect());
    });

    // 2. cluster_misc_state & ClusterAndRuntimeStore sub-fields
    assert_b("cluster_misc_state.char_settings", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .char_settings
            .entry(CHAR_B_ID)
            .or_default()
            .insert("k".into(), PyRep::Integer(1));
    });
    assert_a("cluster_misc_state.char_settings A", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .char_settings
            .entry(CHAR_A_ID)
            .or_default()
            .insert("k".into(), PyRep::Integer(1));
    });
    assert_b("cluster_misc_state.char_energy", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .char_energy
            .insert(CHAR_B_ID, CharacterEnergyState { current_energy: 50, ..Default::default() });
    });
    assert_a("cluster_misc_state.char_energy A", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .char_energy
            .insert(CHAR_A_ID, CharacterEnergyState { current_energy: 50, ..Default::default() });
    });
    assert_b("cluster_misc_state.used_starter_shipcaster_chars", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .used_starter_shipcaster_chars
            .insert(CHAR_B_ID);
    });
    assert_b("cluster_misc_state.offered_load_slots", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .offered_load_slots
            .insert(CHAR_B_ID, OfferedLoadSlot { slot_key: "b".into(), ..Default::default() });
    });
    assert_b("cluster_misc_state.zarzakh_toll_quotes", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .zarzakh_toll_quotes
            .insert(CHAR_B_ID, PendingZarzakhTollQuote { ship_id: b_ship_id, ..Default::default() });
    });
    assert_b("cluster_misc_state.target_cooldowns", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .target_cooldowns
            .insert(CORP_B_STRUCTURE_ID, 42);
    });
    assert_b("cluster_misc_state.target_charges", &base, &|s| {
        s.cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default)
            .target_charges
            .insert(CORP_B_STRUCTURE_ID, 2);
    });
    assert_a("cluster_misc_state A/global", &base, &|s| {
        let m = s
            .cluster
            .cluster_misc_state
            .get_or_insert_with(PersistedClusterMiscState::default);
        m.used_starter_shipcaster_chars.insert(CHAR_A_ID);
        m.target_cooldowns.insert(a_ship_id, 42);
        m.target_charges.insert(a_ship_id, 2);
        m.persisted_shipcaster_state = true;
    });
    assert_b("clone_vat_bay_state.active_by_ship", &base, &|s| {
        s.cluster.clone_vat_bay_state.active_by_ship.insert(b_ship_id, (b_mod_id, 999));
    });
    assert_a("clone_vat_bay_state.active_by_ship A", &base, &|s| {
        s.cluster.clone_vat_bay_state.active_by_ship.insert(a_ship_id, (a_mod_id, 999));
    });
    assert_b("auto_reloaded_event", &base, &|s| {
        s.cluster.auto_reloaded_event.insert(b_mod_id, (vec![], 218, 1000.0));
    });
    assert_a("auto_reloaded_event A", &base, &|s| {
        s.cluster.auto_reloaded_event.insert(a_mod_id, (vec![], 218, 1000.0));
    });
    assert_b("resculpt_pending", &base, &|s| {
        s.cluster.resculpt_pending.insert(CHAR_B_ID);
    });
    assert_b("char_cohorts", &base, &|s| {
        s.cluster.char_cohorts.insert(CHAR_B_ID, vec!["beta".into()]);
    });
    assert_b("user_cohorts", &base, &|s| {
        s.cluster.user_cohorts.insert(USER_B_ID, vec!["beta".into()]);
    });
    assert_b("character_locks", &base, &|s| {
        s.cluster.character_locks.insert(CHAR_B_ID, "gm".into());
    });
    assert_b("omega_downgraded", &base, &|s| {
        s.cluster.omega_downgraded.insert(USER_B_ID);
    });
    assert_b("localization_qa_wrap_users by USER_B_ID", &base, &|s| {
        s.cluster.localization_qa_wrap_users.insert(USER_B_ID as i64, true);
    });
    assert_a("cluster misc user/char flags A", &base, &|s| {
        s.cluster.resculpt_pending.insert(CHAR_A_ID);
        s.cluster.char_cohorts.insert(CHAR_A_ID, vec!["alpha".into()]);
        s.cluster.user_cohorts.insert(USER_A_ID, vec!["alpha".into()]);
        s.cluster.character_locks.insert(CHAR_A_ID, "gm".into());
        s.cluster.omega_downgraded.insert(USER_A_ID);
        s.cluster.localization_qa_wrap_users.insert(USER_A_ID as i64, true);
    });
    assert_b("user_sessions", &base, &|s| {
        s.cluster
            .user_sessions
            .push(serde_json::json!({"user_id": USER_B_ID, "character_id": CHAR_B_ID}));
    });
    assert_a("user_sessions A", &base, &|s| {
        s.cluster
            .user_sessions
            .push(serde_json::json!({"user_id": USER_A_ID, "character_id": CHAR_A_ID}));
    });
    assert_b("server_runtime_json B changed", &base, &|s| {
        s.cluster.server_runtime_json = Some(
            serde_json::json!({
                "actors": [
                    {"character_id": CHAR_A_ID, "counter": 1},
                    {"character_id": CHAR_B_ID, "counter": 2}
                ]
            })
            .to_string(),
        );
    });
    assert_a("server_runtime_json only A changed", &base, &|s| {
        s.cluster.server_runtime_json = Some(
            serde_json::json!({
                "actors": [
                    {"character_id": CHAR_A_ID, "counter": 2},
                    {"character_id": CHAR_B_ID, "counter": 1}
                ]
            })
            .to_string(),
        );
    });

    // 3. QuasarCommerceStore sub-fields
    assert_b("cosmetics_market skin_listings seller B", &base, &|s| {
        let cm = s.quasar.cosmetics_market.get_or_insert_with(CosmeticsMarketData::default);
        cm.skin_listings.insert(
            "l-b".into(),
            SkinListingRecord { seller_id: CHAR_B_ID as u32, ..Default::default() },
        );
    });
    assert_b("cosmetics_market skin_listings target corp B", &base, &|s| {
        let cm = s.quasar.cosmetics_market.get_or_insert_with(CosmeticsMarketData::default);
        cm.skin_listings.insert(
            "l-corp".into(),
            SkinListingRecord {
                seller_id: CHAR_A_ID as u32,
                target: ListingTargetRecord::Corporation(CORP_B_ID as u64),
                ..Default::default()
            },
        );
    });
    assert_b("cosmetics_market transactions buyer B", &base, &|s| {
        let cm = s.quasar.cosmetics_market.get_or_insert_with(CosmeticsMarketData::default);
        cm.transactions.insert(
            "tx-b".into(),
            CosmeticTransactionRecord { buyer_id: Some(CHAR_B_ID as u32), ..Default::default() },
        );
    });
    assert_a("cosmetics_market A/global", &base, &|s| {
        let cm = s.quasar.cosmetics_market.get_or_insert_with(CosmeticsMarketData::default);
        cm.next_seq = 55;
        cm.skin_listings.insert(
            "l-a".into(),
            SkinListingRecord {
                seller_id: CHAR_A_ID as u32,
                target: ListingTargetRecord::Public,
                ..Default::default()
            },
        );
        cm.transactions.insert(
            "tx-a".into(),
            CosmeticTransactionRecord { buyer_id: Some(CHAR_A_ID as u32), ..Default::default() },
        );
    });
    assert_b("failed_fulfillments", &base, &|s| {
        s.quasar.failed_fulfillments.push(FailedFulfillmentRecord {
            id: 1,
            user_id: USER_B_ID,
            ..Default::default()
        });
    });
    assert_b("targeted_offers", &base, &|s| {
        s.quasar.targeted_offers.insert(
            format!("{USER_B_ID}:offer"),
            TargetedOfferAdminRecord { user_id: USER_B_ID, ..Default::default() },
        );
    });
    assert_b("acl_registry", &base, &|s| {
        s.quasar.acl_registry = Some(vec![QuasarAclRecord {
            acl_id: 10,
            characters: vec![(CHAR_B_ID as u64, 1)],
            ..Default::default()
        }]);
    });
    assert_a("quasar A/global", &base, &|s| {
        s.quasar.failed_fulfillments.push(FailedFulfillmentRecord {
            id: 2,
            user_id: USER_A_ID,
            ..Default::default()
        });
        s.quasar.targeted_offers.insert(
            format!("{USER_A_ID}:offer"),
            TargetedOfferAdminRecord { user_id: USER_A_ID, ..Default::default() },
        );
        s.quasar.acl_registry = Some(vec![QuasarAclRecord {
            acl_id: 11,
            characters: vec![(CHAR_A_ID as u64, 1)],
            ..Default::default()
        }]);
    });

    // 4. PvpFilamentStore / TourneyStore / MobileDeployableRuntimeState sub-fields
    assert_b("pvp event_stats", &base, &|s| {
        s.pvp.event_stats.entry(7).or_default().insert(CHAR_B_ID, (1, 0, 0, 0, 1.0));
    });
    assert_b("pvp active_matches/match_teams/match_eliminated", &base, &|s| {
        s.pvp.active_matches.insert(
            11,
            PvpArenaMatchInstance {
                match_id: 11,
                team_a: vec![CHAR_B_ID],
                ..Default::default()
            },
        );
        s.pvp.match_teams.insert(11, vec![vec![CHAR_B_ID]]);
        s.pvp.match_eliminated.insert(11, [CHAR_B_ID].into_iter().collect());
    });
    assert_b("pvp queue_refund_targets", &base, &|s| {
        s.pvp.queue_refund_targets.insert(55, (CHAR_B_ID, b_ship_id));
    });
    assert_a("pvp A", &base, &|s| {
        s.pvp.event_stats.entry(7).or_default().insert(CHAR_A_ID, (1, 0, 0, 0, 1.0));
        s.pvp.active_matches.insert(
            12,
            PvpArenaMatchInstance {
                match_id: 12,
                team_a: vec![CHAR_A_ID],
                ..Default::default()
            },
        );
        s.pvp.match_teams.insert(12, vec![vec![CHAR_A_ID]]);
        s.pvp.match_eliminated.insert(12, [CHAR_A_ID].into_iter().collect());
        s.pvp.queue_refund_targets.insert(56, (CHAR_A_ID, a_ship_id));
    });
    assert_b("tourney.matches status change", &base, &|s| {
        s.tourney.matches.get_mut(&21).unwrap().red_score = 5;
    });
    assert_b("tourney.ban_sessions scoped to Target B match", &base, &|s| {
        s.tourney.ban_sessions.insert(21_000_001, vec![587]);
    });
    assert_a("tourney A match + ban_sessions", &base, &|s| {
        s.tourney.matches.get_mut(&22).unwrap().red_score = 5;
        s.tourney.ban_sessions.insert(22_000_001, vec![587]);
    });
    assert_b("deployable damaged_items", &base, &|s| {
        s.deployable.damaged_items.insert(
            b_ship_id,
            PersistedDamageRecord { shield_hp: 50.0, ..Default::default() },
        );
    });
    assert_b("deployable cloak_activated_unix", &base, &|s| {
        s.deployable.cloak_activated_unix.insert(b_ship_id, 123);
    });
    assert_b("deployable mmju_spools", &base, &|s| {
        s.deployable.mmju_spools.insert(
            b_ship_id,
            MmjuSpoolEntry {
                char_id: CHAR_B_ID,
                ship_id: b_ship_id,
                ..Default::default()
            },
        );
    });
    assert_b("deployable pending_decloak_notifications", &base, &|s| {
        s.deployable.pending_decloak_notifications.push(PendingDecloakNotice {
            char_id: CHAR_B_ID,
            ship_id: b_ship_id,
            module_id: b_mod_id,
        });
    });
    assert_a("deployable A", &base, &|s| {
        s.deployable.damaged_items.insert(
            a_ship_id,
            PersistedDamageRecord { shield_hp: 50.0, ..Default::default() },
        );
        s.deployable.cloak_activated_unix.insert(a_ship_id, 123);
        s.deployable.mmju_spools.insert(
            a_ship_id,
            MmjuSpoolEntry {
                char_id: CHAR_A_ID,
                ship_id: a_ship_id,
                ..Default::default()
            },
        );
        s.deployable.pending_decloak_notifications.push(PendingDecloakNotice {
            char_id: CHAR_A_ID,
            ship_id: a_ship_id,
            module_id: a_mod_id,
        });
    });
}
