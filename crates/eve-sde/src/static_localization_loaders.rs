//! On-demand localization FSD pickle loaders (`res:/localizationfsd/localization_fsd_*.pickle`),
//! FSD `messageID` multi-language resolution, market-group descriptions, and belt-ore loaders.

#![forbid(unsafe_code)]

use super::*;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

/// A label metadata record from `res:/localizationfsd/localization_fsd_main.pickle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationLabelEntry {
    pub message_id: u32,
    pub full_path: String,
    pub label: String,
}

/// Decoded contents of `res:/localizationfsd/localization_fsd_main.pickle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationMainBundle {
    pub languages: Vec<String>,
    pub labels: HashMap<u32, LocalizationLabelEntry>,
}

/// Normalizes an ESI / FSD language tag (`en`, `en-us`, `de`, `es`, `fr`, `ja`, `ko`, `ru`, `zh`)
/// to its canonical `localization_fsd_<lang>.pickle` suffix.
pub fn normalize_localization_fsd_lang(lang: &str) -> Option<&'static str> {
    let primary = lang
        .split(',')
        .next()
        .unwrap_or(lang)
        .split(';')
        .next()
        .unwrap_or(lang)
        .trim();
    if primary.eq_ignore_ascii_case("en") || primary.eq_ignore_ascii_case("en-us") {
        return Some("en-us");
    }
    let code = primary.split('-').next().unwrap_or(primary);
    match code.to_ascii_lowercase().as_str() {
        "en" => Some("en-us"),
        "de" => Some("de"),
        "es" => Some("es"),
        "fr" => Some("fr"),
        "ja" => Some("ja"),
        "ko" => Some("ko"),
        "ru" => Some("ru"),
        "zh" => Some("zh"),
        _ => None,
    }
}

/// Decodes a Python protocol-0 `V` (`raw-unicode-escape`) line into a Rust `String`.
fn decode_pickle_raw_unicode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut skip = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if b != b'\\' {
            out.push(char::from(b));
            continue;
        }
        let bs_count = bytes[i..].iter().take_while(|&&c| c == b'\\').count();
        let esc_width = if bs_count % 2 == 1 {
            match bytes.get(i + bs_count) {
                Some(&b'u') => 4,
                Some(&b'U') => 8,
                _ => 0,
            }
        } else {
            0
        };
        if esc_width > 0 {
            let hex_start = i + bs_count + 1;
            if let Some(cp) = bytes
                .get(hex_start..hex_start + esc_width)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|h| u32::from_str_radix(h, 16).ok())
            {
                for _ in 0..(bs_count - 1) {
                    out.push('\\');
                }
                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                skip = bs_count + esc_width;
                continue;
            }
        }
        for _ in 0..bs_count {
            out.push('\\');
        }
        skip = bs_count - 1;
    }
    out
}

fn parse_localization_fsd_lang_bytes(
    raw_loc: &[u8],
    needed_ids: Option<&HashSet<u32>>,
) -> HashMap<u32, String> {
    let cap = needed_ids.map_or(308_657, HashSet::len);
    let mut out = HashMap::with_capacity(cap);
    let lines: Vec<&[u8]> = raw_loc.split(|&b| b == b'\n').collect();
    for pair in lines.windows(2) {
        let Some(id_bytes) = pair[0]
            .strip_prefix(b"sI")
            .or_else(|| pair[0].strip_prefix(b"I"))
        else {
            continue;
        };
        let Some(val_bytes) = pair[1].strip_prefix(b"(V") else {
            continue;
        };
        let Ok(id_str) = std::str::from_utf8(id_bytes) else {
            continue;
        };
        let Ok(msg_id) = id_str.parse::<u32>() else {
            continue;
        };
        if let Some(filter) = needed_ids {
            if !filter.contains(&msg_id) {
                continue;
            }
        }
        out.insert(msg_id, decode_pickle_raw_unicode(val_bytes));
        if let Some(filter) = needed_ids {
            if out.len() == filter.len() {
                break;
            }
        }
    }
    out
}

#[derive(Clone)]
enum MainPickleVal {
    Int(i64),
    Str(String),
    List(Vec<MainPickleVal>),
    Dict(Vec<(MainPickleVal, MainPickleVal)>),
}

fn parse_localization_fsd_main_bytes(raw_main: &[u8]) -> Result<LocalizationMainBundle, SdeError> {
    let mut stack: Vec<MainPickleVal> = Vec::with_capacity(16);
    let mut memo: HashMap<u32, MainPickleVal> = HashMap::with_capacity(64);
    for raw_line in raw_main.split(|&b| b == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let prefix_len = line.iter().take_while(|&&c| c == b'a' || c == b's').count();
        for &op in &line[..prefix_len] {
            if op == b'a' {
                if let Some(val) = stack.pop() {
                    if let Some(MainPickleVal::List(items)) = stack.last_mut() {
                        items.push(val);
                    }
                }
            } else if let (Some(val), Some(key)) = (stack.pop(), stack.pop()) {
                if let Some(MainPickleVal::Dict(entries)) = stack.last_mut() {
                    entries.push((key, val));
                }
            }
        }
        let rest = &line[prefix_len..];
        if rest.is_empty() || rest == b"." {
            continue;
        }
        if let Some(memo_bytes) = rest.strip_prefix(b"(dp") {
            let val = MainPickleVal::Dict(Vec::new());
            if let Ok(mid) = std::str::from_utf8(memo_bytes).unwrap_or("").parse::<u32>() {
                if mid <= 64 {
                    memo.insert(mid, val.clone());
                }
            }
            stack.push(val);
        } else if rest == b"(d" {
            stack.push(MainPickleVal::Dict(Vec::new()));
        } else if let Some(memo_bytes) = rest.strip_prefix(b"(lp") {
            let val = MainPickleVal::List(Vec::new());
            if let Ok(mid) = std::str::from_utf8(memo_bytes).unwrap_or("").parse::<u32>() {
                if mid <= 64 {
                    memo.insert(mid, val.clone());
                }
            }
            stack.push(val);
        } else if rest == b"(l" {
            stack.push(MainPickleVal::List(Vec::new()));
        } else if let Some(int_bytes) = rest.strip_prefix(b"I") {
            let n = std::str::from_utf8(int_bytes)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| SdeError::InvalidSchema("invalid main pickle int".into()))?;
            stack.push(MainPickleVal::Int(n));
        } else if rest.starts_with(b"S'") || rest.starts_with(b"S\"") {
            let q = rest[1];
            let inner = rest
                .get(2..)
                .and_then(|s| s.strip_suffix(&[q]))
                .and_then(|s| std::str::from_utf8(s).ok())
                .ok_or_else(|| SdeError::InvalidSchema("invalid main pickle string".into()))?;
            stack.push(MainPickleVal::Str(inner.to_string()));
        } else if let Some(memo_bytes) = rest.strip_prefix(b"p") {
            if let Ok(mid) = std::str::from_utf8(memo_bytes).unwrap_or("").parse::<u32>() {
                if mid <= 64 {
                    if let Some(top) = stack.last() {
                        memo.insert(mid, top.clone());
                    }
                }
            }
        } else if let Some(memo_bytes) = rest.strip_prefix(b"g") {
            let mid = std::str::from_utf8(memo_bytes)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| SdeError::InvalidSchema("invalid main pickle get".into()))?;
            let val = memo
                .get(&mid)
                .cloned()
                .ok_or_else(|| SdeError::InvalidSchema(format!("missing memo {mid}")))?;
            stack.push(val);
        }
    }
    let Some(MainPickleVal::Dict(top_entries)) = stack.pop() else {
        return Err(SdeError::InvalidSchema("main pickle root must be dict".into()));
    };
    let mut languages = Vec::new();
    let mut labels = HashMap::new();
    for (k, v) in top_entries {
        let MainPickleVal::Str(key_str) = k else {
            continue;
        };
        if key_str == "languages" {
            if let MainPickleVal::List(items) = v {
                for item in items {
                    if let MainPickleVal::Str(s) = item {
                        languages.push(s);
                    }
                }
            }
        } else if key_str == "labels" {
            if let MainPickleVal::Dict(entries) = v {
                labels.reserve(entries.len());
                for (lk, lv) in entries {
                    let MainPickleVal::Int(msg_id_i64) = lk else {
                        continue;
                    };
                    let MainPickleVal::Dict(fields) = lv else {
                        continue;
                    };
                    let mut full_path = String::new();
                    let mut label = String::new();
                    let mut field_mid = msg_id_i64 as u32;
                    for (fk, fv) in fields {
                        if let MainPickleVal::Str(fks) = fk {
                            match (fks.as_str(), fv) {
                                ("FullPath", MainPickleVal::Str(s)) => full_path = s,
                                ("label", MainPickleVal::Str(s)) => label = s,
                                ("messageID", MainPickleVal::Int(n)) => field_mid = n as u32,
                                _ => {}
                            }
                        }
                    }
                    labels.insert(
                        msg_id_i64 as u32,
                        LocalizationLabelEntry {
                            message_id: field_mid,
                            full_path,
                            label,
                        },
                    );
                }
            }
        }
    }
    Ok(LocalizationMainBundle { languages, labels })
}

struct FsdLocalizationIds {
    types: HashMap<TypeId, (u32, Option<u32>)>,
    groups: HashMap<GroupId, u32>,
    categories: HashMap<CategoryId, u32>,
    market_groups: HashMap<u32, (u32, Option<u32>)>,
}

impl SdeRegistry {
    /// Loads `res:/localizationfsd/localization_fsd_main.pickle` (`9` languages + `31,872` labels)
    /// (`sde:res:/localizationfsd/localization_fsd_main.pickle`).
    pub fn load_localization_main(&self) -> Result<LocalizationMainBundle, SdeError> {
        let raw =
            self.read_fsd_static_bytes("res:/localizationfsd/localization_fsd_main.pickle")?;
        parse_localization_fsd_main_bytes(&raw)
    }

    /// Loads every localized `messageID -> text` entry for `lang` (`de`, `en-us`, `es`, `fr`,
    /// `ja`, `ko`, `ru`, `zh`) from `res:/localizationfsd/localization_fsd_<lang>.pickle`.
    pub fn load_all_localized_messages(&self, lang: &str) -> Result<HashMap<u32, String>, SdeError> {
        let norm = normalize_localization_fsd_lang(lang).ok_or_else(|| {
            SdeError::InvalidSchema(format!("unsupported localizationFSD language '{lang}'"))
        })?;
        let res_path = format!("res:/localizationfsd/localization_fsd_{norm}.pickle");
        let raw_loc = self.read_fsd_static_bytes(&res_path)?;
        Ok(parse_localization_fsd_lang_bytes(&raw_loc, None))
    }

    /// Loads a requested subset of localized messages by `messageID` for `lang`.
    pub fn load_localized_messages(
        &self,
        lang: &str,
        needed_ids: &HashSet<u32>,
    ) -> Result<HashMap<u32, String>, SdeError> {
        let norm = normalize_localization_fsd_lang(lang).ok_or_else(|| {
            SdeError::InvalidSchema(format!("unsupported localizationFSD language '{lang}'"))
        })?;
        if needed_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let res_path = format!("res:/localizationfsd/localization_fsd_{norm}.pickle");
        let raw_loc = self.read_fsd_static_bytes(&res_path)?;
        Ok(parse_localization_fsd_lang_bytes(&raw_loc, Some(needed_ids)))
    }

    /// Loads every localized English message entry (`308,657` `messageID -> text` pairs) from
    /// `res:/localizationfsd/localization_fsd_en-us.pickle` (`sde:res:/localizationfsd/localization_fsd_en-us.pickle`).
    pub fn load_all_localized_messages_en_us(&self) -> Result<HashMap<u32, String>, SdeError> {
        self.load_all_localized_messages("en-us")
    }

    /// Loads a requested subset of localized English messages by `messageID`.
    pub fn load_localized_messages_en_us(
        &self,
        needed_ids: &HashSet<u32>,
    ) -> Result<HashMap<u32, String>, SdeError> {
        self.load_localized_messages("en-us", needed_ids)
    }

    /// Looks up a single localized string by `lang` and `messageID` using an on-demand per-locale cache.
    pub fn localized_message(&self, lang: &str, message_id: u32) -> Option<String> {
        let norm = normalize_localization_fsd_lang(lang)?;
        static LOC_BY_LANG: OnceLock<Mutex<HashMap<&'static str, Arc<HashMap<u32, String>>>>> =
            OnceLock::new();
        let cache = LOC_BY_LANG.get_or_init(|| Mutex::new(HashMap::new()));
        let arc_map = {
            let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(norm).cloned()
        }
        .unwrap_or_else(|| {
            let loaded = Arc::new(self.load_all_localized_messages(norm).unwrap_or_default());
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(guard.entry(norm).or_insert(loaded))
        });
        arc_map.get(&message_id).cloned()
    }

    /// Looks up a single localized English string by `messageID`.
    pub fn localized_message_en_us(&self, message_id: u32) -> Option<String> {
        self.localized_message("en-us", message_id)
    }

    fn fsd_localization_ids(&self) -> &'static FsdLocalizationIds {
        static IDS: OnceLock<FsdLocalizationIds> = OnceLock::new();
        IDS.get_or_init(|| {
            let types = self
                .load_raw_types_fsdbinary()
                .map(|(_, m)| {
                    m.into_iter()
                        .map(|(tid, r)| (tid, (r.type_name_id, r.description_id)))
                        .collect()
                })
                .unwrap_or_default();
            let groups = self
                .read_cfsd_bytes("res:/staticdata/groups.fsdbinary", 32)
                .map(|(raw, recs)| {
                    recs.into_iter()
                        .map(|rec| (u32_le(&raw, rec), u32_le(&raw, rec + 12)))
                        .collect()
                })
                .unwrap_or_default();
            let categories = self
                .read_cfsd_bytes("res:/staticdata/categories.fsdbinary", 48)
                .map(|(raw, recs)| {
                    recs.into_iter()
                        .map(|rec| (u32_le(&raw, rec), u32_le(&raw, rec + 20)))
                        .collect()
                })
                .unwrap_or_default();
            let market_groups = self
                .read_cfsd_bytes("res:/staticdata/marketgroups.fsdbinary", 28)
                .map(|(raw, recs)| {
                    recs.into_iter()
                        .map(|rec| {
                            let mgid = u32_le(&raw, rec);
                            let name_id = u32_le(&raw, rec + 12);
                            let mask = u32_le(&raw, rec + 24);
                            let desc_id = ((mask & 1) != 0)
                                .then(|| u32_le(&raw, rec + 4))
                                .filter(|&v| v > 0);
                            (mgid, (name_id, desc_id))
                        })
                        .collect()
                })
                .unwrap_or_default();
            FsdLocalizationIds {
                types,
                groups,
                categories,
                market_groups,
            }
        })
    }

    pub fn localized_type_name(&self, lang: &str, type_id: TypeId) -> Option<String> {
        let &(name_id, _) = self.fsd_localization_ids().types.get(&type_id)?;
        self.localized_message(lang, name_id).filter(|s| !s.is_empty())
    }

    pub fn localized_type_description(&self, lang: &str, type_id: TypeId) -> Option<String> {
        let desc_id = self
            .fsd_localization_ids()
            .types
            .get(&type_id)
            .and_then(|&(_, d)| d)
            .or_else(|| self.types.get(&type_id).and_then(|t| t.description_id))?;
        self.localized_message(lang, desc_id).filter(|s| !s.is_empty())
    }

    pub fn localized_group_name(&self, lang: &str, group_id: GroupId) -> Option<String> {
        let &name_id = self.fsd_localization_ids().groups.get(&group_id)?;
        self.localized_message(lang, name_id).filter(|s| !s.is_empty())
    }

    pub fn localized_category_name(&self, lang: &str, category_id: CategoryId) -> Option<String> {
        let &name_id = self.fsd_localization_ids().categories.get(&category_id)?;
        self.localized_message(lang, name_id).filter(|s| !s.is_empty())
    }

    pub fn localized_market_group_name(&self, lang: &str, market_group_id: u32) -> Option<String> {
        let &(name_id, _) = self.fsd_localization_ids().market_groups.get(&market_group_id)?;
        self.localized_message(lang, name_id).filter(|s| !s.is_empty())
    }

    pub fn localized_market_group_description(
        &self,
        lang: &str,
        market_group_id: u32,
    ) -> Option<String> {
        let &(_, desc_id) = self.fsd_localization_ids().market_groups.get(&market_group_id)?;
        self.localized_message(lang, desc_id?).filter(|s| !s.is_empty())
    }

    /// Loads localized English descriptions from `res:/staticdata/marketgroups.fsdbinary`
    /// (`descriptionID`) resolved via `res:/localizationfsd/localization_fsd_en-us.pickle`.
    pub fn load_market_group_descriptions(&self) -> Result<HashMap<u32, String>, SdeError> {
        let (_, recs_raw) = self.read_cfsd_bytes("res:/staticdata/marketgroups.fsdbinary", 28)?;
        let raw = self.read_fsd_static_bytes("res:/staticdata/marketgroups.fsdbinary")?;
        let mut desc_ids_by_mg = Vec::with_capacity(recs_raw.len());
        let mut needed_ids = HashSet::new();
        for rec in recs_raw {
            let mgid = u32_le(&raw, rec);
            let desc_id = u32_le(&raw, rec + 4);
            let mask = u32_le(&raw, rec + 24);
            if (mask & 1) != 0 && desc_id > 0 {
                desc_ids_by_mg.push((mgid, desc_id));
                needed_ids.insert(desc_id);
            }
        }
        let localized = self.load_localized_messages_en_us(&needed_ids).unwrap_or_default();
        let mut out = HashMap::with_capacity(desc_ids_by_mg.len());
        for (mgid, desc_id) in desc_ids_by_mg {
            if let Some(text) = localized.get(&desc_id).filter(|t| !t.is_empty()) {
                out.insert(mgid, text.clone());
            }
        }
        Ok(out)
    }

    /// Returns the cached localized English description for a market group ID, if present.
    pub fn market_group_description(&self, market_group_id: u32) -> Option<String> {
        static MG_DESCS: OnceLock<HashMap<u32, String>> = OnceLock::new();
        let map = MG_DESCS.get_or_init(|| self.load_market_group_descriptions().unwrap_or_default());
        map.get(&market_group_id).cloned()
    }
}
