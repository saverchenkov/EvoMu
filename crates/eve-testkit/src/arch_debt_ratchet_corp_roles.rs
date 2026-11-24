//! Corporation-role architecture-debt ratchet counters and unit tests (`#1194`).

#![forbid(unsafe_code)]

use crate::arch_debt_ratchet::{
    count_fn_decl, count_whole_word, count_word_lparen, has_lparen_after_prefix,
    has_word_boundary_after, skip_ws,
};

pub(crate) fn count_member_base_roles_calls(text: &str) -> usize {
    if !text.contains("member_base_roles") {
        return 0;
    }
    const STEM: &str = "member_base_roles";
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices(STEM) {
        let mut dot = i;
        while dot > 0 && b[dot - 1].is_ascii_whitespace() {
            dot -= 1;
        }
        if dot == 0 || b[dot - 1] != b'.' {
            continue;
        }
        let mut end = i + STEM.len();
        if end + 4 <= b.len() && &b[end..end + 4] == b"_opt" {
            end += 4;
        }
        let lp = skip_ws(b, end);
        if lp < b.len() && b[lp] == b'(' {
            count += 1;
        }
    }
    count
}

pub(crate) fn count_fn_is_npc_corp(text: &str) -> usize {
    count_fn_decl(text, "is_npc_corp", |rest| {
        if !rest.starts_with(b"is_npc_corp") {
            return false;
        }
        let mut pos = "is_npc_corp".len();
        if rest[pos..].starts_with(b"oration") {
            pos += "oration".len();
        }
        if rest[pos..].starts_with(b"_id") {
            pos += "_id".len();
        }
        has_lparen_after_prefix(rest, pos)
    })
}

pub(crate) fn count_fn_is_excluded_corp_member(text: &str) -> usize {
    count_fn_decl(text, "is_excluded_corp_member", |rest| {
        rest.starts_with(b"is_excluded_corp_member")
            && has_lparen_after_prefix(rest, "is_excluded_corp_member".len())
    })
}

pub(crate) fn count_legacy_effective_corp_roles_at_fallback(text: &str) -> usize {
    count_word_lparen(text, "effective_corp_roles_at")
}

pub(crate) fn count_effective_corp_member_roles_call_sites(text: &str) -> usize {
    count_word_lparen(text, "effective_corp_member_roles")
}

pub(crate) fn count_request_corp_role_mask_reads(text: &str) -> usize {
    count_whole_word(text, "corp_role_mask") + count_dot_field_access(text, "bitmask")
}

fn count_dot_field_access(text: &str, field: &str) -> usize {
    if !text.contains(field) {
        return 0;
    }
    let b = text.as_bytes();
    let flen = field.len();
    let mut count = 0usize;
    for (i, _) in text.match_indices(field) {
        if !has_word_boundary_after(b, i + flen) {
            continue;
        }
        let mut dot = i;
        while dot > 0 && b[dot - 1].is_ascii_whitespace() {
            dot -= 1;
        }
        if dot > 0 && b[dot - 1] == b'.' {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::arch_debt_ratchet::{scan_arch_debt_in_dir, ArchDebtRatchet};

    fn make_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "evomu_arch_debt_corp_roles_test_{label}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn arch_debt_ratchet_1194_duplicate_is_npc_corp_and_excluded_member_helpers() {
        let tmp = make_temp_dir("1194_f3_corp_helpers");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("services/bad.rs"),
            r#"
// fn is_npc_corporation_id(ctx: &ServerContext, corp_id: i64) -> bool { false }
// fn is_excluded_corp_member(corp: &CorporationRecord, char_id: i64) -> bool { false }
const LIT: &str = "fn is_npc_corp(id: i64) {} fn is_excluded_corp_member(c: &C) {}";
fn is_npc_corp(id: i64) -> bool { false }
fn is_npc_corp_id(ctx: &ServerContext, id: i64) -> bool { false }
pub(crate) fn is_npc_corporation(ctx: &ServerContext, id: i64) -> bool { false }
fn is_npc_corporation_id (ctx: &ServerContext, id: i64) -> bool { false }
fn is_excluded_corp_member(corp: &CorporationRecord, char_id: i64) -> bool { false }
pub fn is_excluded_corp_member (corp: &CorporationRecord, char_id: i64) -> bool { false }
// Non-matching names:
fn is_npc_corp_helper(id: i64) -> bool { false }
fn is_excluded_corp_member_v2(id: i64) -> bool { false }
#[cfg(test)]
mod tests {
    fn is_npc_corp(id: i64) -> bool { false }
    fn is_excluded_corp_member(id: i64) -> bool { false }
}
"#,
        )
        .expect("write services/bad.rs");

        let ratchet = ArchDebtRatchet::load_default().expect("load default arch_debt_ratchet.json");
        assert!(ratchet.patterns["duplicate_is_npc_corp_helpers"].zero_required);
        assert!(ratchet.patterns["duplicate_is_excluded_corp_member_helpers"].zero_required);
        assert!(ratchet.patterns["duplicate_is_npc_corp_helpers"].files.is_empty());
        assert!(ratchet.patterns["duplicate_is_excluded_corp_member_helpers"].files.is_empty());

        let report = scan_arch_debt_in_dir(&src).expect("scan 1194 f3 helpers");
        let npc_entry = &report.patterns["duplicate_is_npc_corp_helpers"];
        assert_eq!(npc_entry.occurrences, 4);
        assert_eq!(npc_entry.per_file.get("services/bad.rs"), Some(&4));

        let excl_entry = &report.patterns["duplicate_is_excluded_corp_member_helpers"];
        assert_eq!(excl_entry.occurrences, 2);
        assert_eq!(excl_entry.per_file.get("services/bad.rs"), Some(&2));

        let err = ratchet
            .assert_ratchet(&report)
            .expect_err("zero_required F3 ratchets must reject duplicate helpers");
        assert!(
            err.contains("duplicate_is_npc_corp_helpers")
                || err.contains("duplicate_is_excluded_corp_member_helpers"),
            "unexpected error message: {err}"
        );

        let clean_src = tmp.join("clean_src");
        std::fs::create_dir_all(clean_src.join("services")).expect("create clean services dir");
        std::fs::write(
            clean_src.join("services/clean.rs"),
            "pub fn check(sde: &SdeRegistry, corp: &CorporationRecord, cid: i64) -> bool {\n    eve_db::is_npc_corporation(sde, cid) || eve_db::is_excluded_corp_member(corp, cid)\n}\n",
        )
        .expect("write clean.rs");
        let clean_report = ratchet
            .verify_dir(&clean_src)
            .expect("clean source using canonical eve_db helpers must pass at 0");
        assert_eq!(clean_report.patterns["duplicate_is_npc_corp_helpers"].occurrences, 0);
        assert_eq!(clean_report.patterns["duplicate_is_excluded_corp_member_helpers"].occurrences, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_1194_legacy_effective_corp_roles_at_fallback() {
        let tmp = make_temp_dir("1194_f1_effective_corp_roles_at");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("services/bad.rs"),
            r#"
// snap.effective_corp_roles_at(cid, char_id, None, session.corp_role, now)
const LIT: &str = "snap.effective_corp_roles_at(cid, char_id, None, 0, now)";
pub fn f(snap: &S, cid: i64, char_id: i64) -> (bool, u64) {
    let a = snap.effective_corp_roles_at(cid, char_id, None, 0, 0);
    let b = effective_corp_roles_at (snap, cid, char_id, None, 0, 0);
    let c = snap.effective_corp_roles_at_db(cid, char_id, None, 0);
    (a.0 && b.0 && c.0, a.1 | b.1 | c.1)
}
#[cfg(test)]
mod tests {
    fn t(snap: &S) { let _ = snap.effective_corp_roles_at(1, 2, None, 0, 0); }
}
"#,
        )
        .expect("write services/bad.rs");

        let ratchet = ArchDebtRatchet::load_default().expect("load default arch_debt_ratchet.json");
        let rule = &ratchet.patterns["legacy_effective_corp_roles_at_fallback"];
        assert!(rule.zero_required);
        assert_eq!(rule.owning_issue, Some(1194));
        assert!(rule.files.is_empty());

        let report = scan_arch_debt_in_dir(&src).expect("scan 1194 f1 legacy fallback");
        let entry = &report.patterns["legacy_effective_corp_roles_at_fallback"];
        assert_eq!(entry.occurrences, 2);
        assert_eq!(entry.per_file.get("services/bad.rs"), Some(&2));

        let err = ratchet
            .assert_ratchet(&report)
            .expect_err("zero_required F1 ratchet must reject legacy effective_corp_roles_at");
        assert!(
            err.contains("legacy_effective_corp_roles_at_fallback"),
            "unexpected error message: {err}"
        );

        let clean_src = tmp.join("clean_src");
        std::fs::create_dir_all(clean_src.join("services")).expect("create clean services dir");
        std::fs::write(
            clean_src.join("services/clean.rs"),
            "pub fn check(snap: &S, cid: i64, char_id: i64, now: u64) -> (bool, u64) {\n    snap.effective_corp_roles_at_db(cid, char_id, None, now)\n}\n",
        )
        .expect("write clean.rs");
        let clean_report = ratchet
            .verify_dir(&clean_src)
            .expect("clean source using effective_corp_roles_at_db must pass at 0");
        assert_eq!(clean_report.patterns["legacy_effective_corp_roles_at_fallback"].occurrences, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_1194_effective_corp_member_roles_call_sites() {
        let tmp = make_temp_dir("1194_t3b2_member_roles");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("services/corp_registry_ops.rs"),
            r#"
// effective_corp_member_roles(snap, corp_id, char_id, 0, now)
const LIT: &str = "effective_corp_member_roles(snap, corp_id, char_id, 0, now)";
pub fn f(snap: &S, cid: i64, char_id: i64) -> (bool, u64) {
    let a = effective_corp_member_roles(snap, cid, char_id, 0, 0);
    let b = effective_corp_member_roles (snap, cid, char_id, 0, 0);
    let c = snap.effective_corp_roles_at_db(cid, char_id, None, 0);
    (a.0 && b.0 && c.0, a.1 | b.1 | c.1)
}
#[cfg(test)]
mod tests {
    fn t(snap: &S) { let _ = effective_corp_member_roles(snap, 1, 2, 0, 0); }
}
"#,
        )
        .expect("write services/corp_registry_ops.rs");

        let ratchet = ArchDebtRatchet::load_default().expect("load default arch_debt_ratchet.json");
        let rule = &ratchet.patterns["effective_corp_member_roles_call_sites"];
        assert!(!rule.zero_required);
        assert_eq!(rule.owning_issue, Some(1194));
        assert_eq!(rule.files.get("services/corp_registry_ops.rs"), Some(&1));

        let report = scan_arch_debt_in_dir(&src).expect("scan effective_corp_member_roles");
        let entry = &report.patterns["effective_corp_member_roles_call_sites"];
        assert_eq!(entry.occurrences, 2);
        assert_eq!(entry.per_file.get("services/corp_registry_ops.rs"), Some(&2));

        let err = ratchet
            .assert_ratchet(&report)
            .expect_err("ratchet must reject growth above baseline for effective_corp_member_roles");
        assert!(
            err.contains("effective_corp_member_roles_call_sites"),
            "unexpected error message: {err}"
        );

        let clean_src = tmp.join("clean_src");
        std::fs::create_dir_all(clean_src.join("services")).expect("create clean services dir");
        std::fs::write(
            clean_src.join("services/corp_registry_ops.rs"),
            "pub fn check(snap: &S, cid: i64, char_id: i64, now: u64) -> (bool, u64) {\n    snap.effective_corp_roles_at_db(cid, char_id, None, now)\n}\n",
        )
        .expect("write clean corp_registry_ops.rs");
        let clean_report = ratchet
            .verify_dir(&clean_src)
            .expect("shrunk source using effective_corp_roles_at_db must pass");
        assert_eq!(
            clean_report.patterns["effective_corp_member_roles_call_sites"].occurrences,
            0
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_1194_request_corp_role_mask_reads() {
        let tmp = make_temp_dir("1194_t3b2_request_role_mask");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("services/quasar_extra.rs"),
            r#"
// ctx.corp_role_mask | roles.bitmask
const LIT: &str = "ctx.corp_role_mask | roles.bitmask";
pub fn f(ctx: &Ctx, roles: &Roles) -> u64 {
    let a = ctx.corp_role_mask;
    let b = roles . bitmask;
    let c = roles.bitmask_extra;
    a | b | c
}
#[cfg(test)]
mod tests {
    fn t(ctx: &Ctx, roles: &Roles) -> u64 { ctx.corp_role_mask | roles.bitmask }
}
"#,
        )
        .expect("write services/quasar_extra.rs");

        let ratchet = ArchDebtRatchet::load_default().expect("load default arch_debt_ratchet.json");
        let rule = &ratchet.patterns["request_corp_role_mask_reads"];
        assert!(!rule.zero_required);
        assert_eq!(rule.owning_issue, Some(1194));
        assert!(!rule.files.contains_key("services/quasar_extra.rs"));

        let report = scan_arch_debt_in_dir(&src).expect("scan request_corp_role_mask_reads");
        let entry = &report.patterns["request_corp_role_mask_reads"];
        assert_eq!(entry.occurrences, 2);
        assert_eq!(entry.per_file.get("services/quasar_extra.rs"), Some(&2));

        let err = ratchet
            .assert_ratchet(&report)
            .expect_err("ratchet must reject un-baselined request corp_role_mask/.bitmask read");
        assert!(
            err.contains("request_corp_role_mask_reads"),
            "unexpected error message: {err}"
        );

        let clean_src = tmp.join("clean_src");
        std::fs::create_dir_all(clean_src.join("services")).expect("create clean services dir");
        std::fs::write(
            clean_src.join("services/quasar_extra.rs"),
            "pub fn check(snap: &S, cid: i64, char_id: i64, now: u64) -> (bool, u64) {\n    snap.effective_corp_roles_at_db(cid, char_id, None, now)\n}\n",
        )
        .expect("write clean quasar_extra.rs");
        let clean_report = ratchet
            .verify_dir(&clean_src)
            .expect("clean source without request role mask reads must pass");
        assert_eq!(clean_report.patterns["request_corp_role_mask_reads"].occurrences, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
