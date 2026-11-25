//! Telemetry summary structs, markdown formatters, and counter ledger state for
//! [`super::delta_item_switch`] (`#931` Track 3B / 3C-1 / 3C-2 / `"tick"`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;

use super::delta_item_outside_rpc::{format_by_scope_markdown, SwitchScopeOutcomeCounts};
use super::delta_item_switch::{
    CanonicalChangeDict, ItemWireFingerprint, ProjectedDedupeEntry, ProjectedItemOpKind,
    ProjectedItemSwitchConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOriginScope {
    Dispatch,
    Tick,
    Neither,
}

impl CompareOriginScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Tick => "tick",
            Self::Neither => "neither",
        }
    }
}

impl std::fmt::Display for CompareOriginScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareByScope<T> {
    pub dispatch: T,
    pub tick: T,
    pub neither: T,
}

#[cfg(test)]
tokio::task_local! { static COMPARE_TICK_SCOPE: (); }

#[cfg(test)]
pub(crate) async fn with_compare_tick_scope<F: std::future::Future<Output = R>, R>(fut: F) -> R {
    COMPARE_TICK_SCOPE.scope((), fut).await
}

fn in_compare_tick_scope() -> bool {
    #[cfg(test)]
    {
        COMPARE_TICK_SCOPE.try_with(|_| ()).is_ok()
    }
    #[cfg(not(test))]
    {
        false
    }
}

pub(crate) fn current_origin_scope_telemetry(
    _ctx: &super::ServerContext,
    in_dispatch: bool,
) -> CompareOriginScope {
    origin_scope_telemetry_for_commit(in_dispatch)
}

pub(crate) fn origin_scope_telemetry_for_commit(in_dispatch: bool) -> CompareOriginScope {
    if in_dispatch {
        CompareOriginScope::Dispatch
    } else if in_compare_tick_scope() {
        CompareOriginScope::Tick
    } else {
        CompareOriginScope::Neither
    }
}

#[cfg(test)]
fn is_valid_rpc_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= 64
        && seg != crate::flight_recorder::UNKNOWN_RPC_LABEL
        && seg != OTHER_RPC_BUCKET
        && seg != OTHER_CLASS_BUCKET
        && !seg
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b == b'.')
}

#[cfg(test)]
pub(crate) fn normalize_dispatch_method_label(service: &str, method: &str) -> String {
    if is_valid_rpc_segment(method) {
        if is_valid_rpc_segment(service) {
            let label = format!("{service}.{method}");
            if label.len() <= 128 {
                return label;
            }
        } else if service.is_empty() {
            return method.to_string();
        }
    }
    crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()
}

#[cfg(test)]
pub(crate) struct DispatchRpcMethodGuard {
    pend_ptr: Option<usize>,
}

#[cfg(test)]
impl Drop for DispatchRpcMethodGuard {
    fn drop(&mut self) {
        if let Some(pend_ptr) = self.pend_ptr {
            super::cache_notifications::pop_dispatch_rpc_method(pend_ptr);
        }
    }
}

#[cfg(test)]
pub(crate) fn push_dispatch_rpc_method_guard(
    pend: &dyn eve_db::lock_rank::AnyRwLock<
        HashMap<i64, Vec<super::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        super::cache_notifications::PendingMachoNotification,
    )>,
    rpc_method: String,
) -> DispatchRpcMethodGuard {
    DispatchRpcMethodGuard {
        pend_ptr: super::cache_notifications::push_dispatch_rpc_method(
            pend, notify_tx, rpc_method,
        ),
    }
}

const MAX_SWITCH_TELEMETRY_BUCKETS: usize = 512;
const MAX_CALL_SITE_TELEMETRY_BUCKETS: usize = 2048;
const MAX_SWITCH_TELEMETRY_SAMPLES: usize = 16;
const OTHER_CLASS_BUCKET: &str = "<other_class>";
const OTHER_RPC_BUCKET: &str = "<other_rpc>";
const OTHER_KEY_BUCKET: &str = "<other_key>";

/// Per-call-site (`file:line`) telemetry counters collected via `#[track_caller]` (`#931` Track 3C-1).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchCallSiteCounts {
    /// First-set helper calls replaced/suppressed because the dedupe fact matched the committed row.
    pub replaced: u64,
    /// First-set helper calls suppressed because a projected commit already set a newer/different
    /// row for `(pilot, item)` (`set + different -> not sent`).
    #[serde(default)]
    pub superseded: u64,
    /// Non-first-set (or non-dispatch/transitioned/corp) helper calls emitted normally.
    pub non_first_set_emitted: u64,
    /// Legacy intermediate-state counter (retained for schema compatibility; zero under superseded suppression).
    pub helper_without_token_with_projected_event: u64,
    /// First-set helper calls with no dedupe fact set (sent normally).
    pub helper_without_token_no_projected_event: u64,
}

/// Deterministic exclusion reason for hand-written item helper emits not covered by first-set projection (`#931` Track 3C-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NotCoveredReason {
    ExplicitEffect,
    FuelReservation,
    NotCharacterOwned,
    OwnerOffline,
    PilotMovedInThisCommit,
    NotInAnRpc,
    LocationNotDockedHangarOrActiveShip,
    FlagNot4Or5,
}

impl NotCoveredReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitEffect => "explicit effect",
            Self::FuelReservation => "fuel reservation",
            Self::NotCharacterOwned => "not character-owned",
            Self::OwnerOffline => "owner offline",
            Self::PilotMovedInThisCommit => "pilot moved in this commit",
            Self::NotInAnRpc => "not in an RPC",
            Self::LocationNotDockedHangarOrActiveShip => "location is not the docked hangar or the active ship",
            Self::FlagNot4Or5 => "flag is not 4 or 5",
        }
    }
}

/// Per-call-site (`file:line`) wallet/account replacement vs pass-through counts (`#931` Track 3C-2).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountSwitchCallSiteCounts {
    pub replaced: u64,
    pub passed_through: u64,
}

/// Wallet/account telemetry summary from `try_handle_legacy_account_change_emit` (`#931` Track 3C-2).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountSwitchSummary {
    pub replaced: u64,
    pub passed_through: u64,
    pub by_call_site: BTreeMap<String, AccountSwitchCallSiteCounts>,
}

impl AccountSwitchSummary {
    /// Returns `true` when any wallet/account switch counter or call-site bucket is non-zero.
    #[inline]
    pub fn has_activity(&self) -> bool {
        self.replaced > 0 || self.passed_through > 0 || !self.by_call_site.is_empty()
    }
}

/// Per-bucket telemetry counters for active item projection switches (`#931` Check #28–#30, #39).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchTelemetryCounts {
    pub emitted: u64,
    pub replaced: u64,
    #[serde(default)]
    pub superseded: u64,
    pub new: u64,
    /// Total first-set helper calls without a matching commit fact
    /// (`helper_without_token_with_projected_event + helper_without_token_no_projected_event`).
    pub helper_without_token: u64,
    /// Legacy intermediate-state counter (zero once superseded rows suppress cleanly).
    pub helper_without_token_with_projected_event: u64,
    /// First-set helper calls without a set fact (`sent`).
    pub helper_without_token_no_projected_event: u64,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
}

/// Observability summary across switched and Step 1 shadow item projection runs (`#931`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectedItemSwitchSummary {
    pub config: ProjectedItemSwitchConfig,
    pub emitted: u64,
    pub replaced: u64,
    #[serde(default)]
    pub superseded: u64,
    pub new: u64,
    pub non_first_set_emitted: u64,
    /// Total first-set helper calls without a commit fact
    /// (`helper_without_token_with_projected_event + helper_without_token_no_projected_event`).
    pub helper_without_token: u64,
    pub helper_without_token_with_projected_event: u64,
    pub helper_without_token_no_projected_event: u64,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
    pub evictions: u64,
    #[serde(default)]
    pub by_scope: ItemCompareByScope<SwitchScopeOutcomeCounts>,
    pub by_class: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_dispatch_method: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_helper: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_class_rpc_helper: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_call_site: BTreeMap<String, SwitchCallSiteCounts>,
    /// Counts of hand-written item helper calls excluded from first-set coverage by reason (`#931` Track 3C-2).
    #[serde(default)]
    pub not_covered_by_reason: BTreeMap<String, u64>,
    /// Per-call-site (`file:line`) counts of hand-written item helper calls excluded from first-set
    /// coverage, keyed by call site -> reason -> count (`#931` Track 3C-2).
    #[serde(default)]
    pub not_covered_by_call_site_reason: BTreeMap<String, BTreeMap<String, u64>>,
    /// Per-call-site (`file:line`) total count of hand-written item events actually sent (`#931` Step 3a).
    #[serde(default)]
    pub hand_sent_by_call_site: BTreeMap<String, u64>,
    /// Bounded diagnostic samples of hand-written item events actually sent (`#931` Step 3a).
    #[serde(default)]
    pub sample_hand_sent: Vec<String>,
    /// Counts of live projected item events grouped by recipient rule (`#931` Step 3a).
    #[serde(default)]
    pub projected_by_rule: BTreeMap<String, u64>,
    /// Counts of live projected item events grouped by recipient rule and RPC method (`#931` Step 3a).
    #[serde(default)]
    pub projected_by_rule_rpc: BTreeMap<String, BTreeMap<String, u64>>,
    /// Wallet/account legacy helper replacement vs pass-through counts (`#931` Track 3C-2).
    #[serde(default)]
    pub account_switches: AccountSwitchSummary,
    pub sample_new: Vec<String>,
    #[serde(default)]
    pub sample_superseded: Vec<String>,
    pub sample_helper_without_token: Vec<String>,
    pub sample_helper_without_token_with_projected_event: Vec<String>,
    pub sample_helper_without_token_no_projected_event: Vec<String>,
    pub sample_dictionary_differs: Vec<String>,
}

impl ProjectedItemSwitchSummary {
    /// Returns `true` when any switch telemetry counter, scope bucket, or call-site bucket recorded activity.
    #[inline]
    pub fn has_activity(&self) -> bool {
        self.emitted > 0
            || self.replaced > 0
            || self.superseded > 0
            || self.new > 0
            || self.non_first_set_emitted > 0
            || self.helper_without_token > 0
            || self.helper_without_token_with_projected_event > 0
            || self.helper_without_token_no_projected_event > 0
            || self.dictionary_same > 0
            || self.dictionary_differs > 0
            || self.evictions > 0
            || self.by_scope.dispatch.has_activity()
            || self.by_scope.tick.has_activity()
            || self.by_scope.neither.has_activity()
            || !self.by_call_site.is_empty()
            || !self.not_covered_by_reason.is_empty()
            || !self.hand_sent_by_call_site.is_empty()
            || !self.projected_by_rule.is_empty()
            || self.account_switches.has_activity()
    }

    /// Returns dispatch RPC methods that triggered `helper_without_token_no_projected_event`.
    pub fn no_projected_event_rpcs(&self) -> BTreeMap<String, u64> {
        self.by_dispatch_method
            .iter()
            .filter_map(|(rpc, counts)| {
                (counts.helper_without_token_no_projected_event > 0)
                    .then_some((rpc.clone(), counts.helper_without_token_no_projected_event))
            })
            .collect()
    }

    /// Renders a markdown report detailing switch config, totals, per-scope outcomes, call sites,
    /// not-covered reasons, wallet counters, and diagnostic samples.
    pub fn format_markdown_report(&self) -> String {
        let mut out = String::new();
        out.push_str("### Item Projection Switch Telemetry Summary\n");
        out.push_str(&format!(
            "- Switches: `char_same_scene_item_mutation={}`, `char_same_scene_item_deleted={}`, `char_same_scene_item_outside_rpc={}`, `char_same_scene_item_fitted={}`, `char_all_items={}`, `corp_all_items={}`\n",
            self.config.char_same_scene_item_mutation,
            self.config.char_same_scene_item_deleted,
            self.config.char_same_scene_item_outside_rpc,
            self.config.char_same_scene_item_fitted,
            self.config.char_all_items,
            self.config.corp_all_items
        ));
        out.push_str(&format!(
            "- Totals: `emitted={}`, `replaced={}`, `superseded={}`, `new={}`, `non_first_set_emitted={}`, `evictions={}`, `dictionary_same={}`, `dictionary_differs={}`\n",
            self.emitted,
            self.replaced,
            self.superseded,
            self.new,
            self.non_first_set_emitted,
            self.evictions,
            self.dictionary_same,
            self.dictionary_differs
        ));
        out.push_str(&format!(
            "- Helper without token (`helper_without_token={}`): `helper_without_token_with_projected_event={}`, `helper_without_token_no_projected_event={}` (no projected event)\n",
            self.helper_without_token,
            self.helper_without_token_with_projected_event,
            self.helper_without_token_no_projected_event
        ));
        format_by_scope_markdown(
            &mut out,
            &self.by_scope.dispatch,
            &self.by_scope.tick,
            &self.by_scope.neither,
        );
        let no_token_rpcs: Vec<(&String, &SwitchTelemetryCounts)> = self
            .by_dispatch_method
            .iter()
            .filter(|(_, c)| c.helper_without_token > 0)
            .collect();
        if !no_token_rpcs.is_empty() {
            out.push_str("\n| Dispatch RPC | `with_projected_event` | `no_projected_event` | Total `helper_without_token` |\n|---|---:|---:|---:|\n");
            for (rpc, c) in no_token_rpcs {
                out.push_str(&format!(
                    "| `{rpc}` | {} | {} | {} |\n",
                    c.helper_without_token_with_projected_event,
                    c.helper_without_token_no_projected_event,
                    c.helper_without_token
                ));
            }
        }
        if !self.by_call_site.is_empty() {
            out.push_str("\n| Call Site | `replaced` | `superseded` | `non_first_set_emitted` | `with_projected_event` | `no_projected_event` |\n|---|---:|---:|---:|---:|---:|\n");
            for (call_site, c) in &self.by_call_site {
                out.push_str(&format!(
                    "| `{call_site}` | {} | {} | {} | {} | {} |\n",
                    c.replaced,
                    c.superseded,
                    c.non_first_set_emitted,
                    c.helper_without_token_with_projected_event,
                    c.helper_without_token_no_projected_event
                ));
            }
        }
        if !self.not_covered_by_reason.is_empty() {
            out.push_str("\n#### Not Covered by Reason\n| Reason | Count |\n|---|---:|\n");
            for (reason, count) in &self.not_covered_by_reason {
                out.push_str(&format!("| `{reason}` | {count} |\n"));
            }
        }
        if !self.not_covered_by_call_site_reason.is_empty() {
            out.push_str("\n#### Not Covered by Call Site & Reason\n| Call Site | Reason | Count |\n|---|---|---:|\n");
            for (site, reasons) in &self.not_covered_by_call_site_reason {
                for (reason, count) in reasons {
                    out.push_str(&format!("| `{site}` | `{reason}` | {count} |\n"));
                }
            }
        }
        if self.account_switches.has_activity() {
            out.push_str(&format!(
                "\n#### Account Change Switch Telemetry\n- Totals: `replaced={}`, `passed_through={}`\n",
                self.account_switches.replaced, self.account_switches.passed_through
            ));
            if !self.account_switches.by_call_site.is_empty() {
                out.push_str("\n| Account Call Site | `replaced` | `passed_through` |\n|---|---:|---:|\n");
                for (site, c) in &self.account_switches.by_call_site {
                    out.push_str(&format!(
                        "| `{site}` | {} | {} |\n",
                        c.replaced, c.passed_through
                    ));
                }
            }
        }
        if !self.hand_sent_by_call_site.is_empty() {
            out.push_str("\n#### Hand-Written Item Events Sent by Call Site\n| Call Site | Sent |\n|---|---:|\n");
            for (site, count) in &self.hand_sent_by_call_site {
                out.push_str(&format!("| `{site}` | {count} |\n"));
            }
        }
        if !self.projected_by_rule.is_empty() {
            out.push_str("\n#### Projected Item Events by Rule\n| Rule | Emitted |\n|---|---:|\n");
            for (rule, count) in &self.projected_by_rule {
                out.push_str(&format!("| `{rule}` | {count} |\n"));
            }
        }
        if !self.projected_by_rule_rpc.is_empty() {
            out.push_str("\n#### Projected Item Events by Rule & RPC\n| Rule | RPC | Emitted |\n|---|---|---:|\n");
            for (rule, rpcs) in &self.projected_by_rule_rpc {
                for (rpc, count) in rpcs {
                    out.push_str(&format!("| `{rule}` | `{rpc}` | {count} |\n"));
                }
            }
        }
        if !self.sample_hand_sent.is_empty() {
            out.push_str("\nHand-written sent samples:\n");
            for sample in &self.sample_hand_sent {
                out.push_str(&format!("- `{sample}`\n"));
            }
        }
        if !self.sample_superseded.is_empty() {
            out.push_str("\nSuperseded samples:\n");
            for sample in &self.sample_superseded {
                out.push_str(&format!("- `{sample}`\n"));
            }
        }
        if !self.sample_dictionary_differs.is_empty() {
            out.push_str("\nDictionary-difference samples:\n");
            for sample in &self.sample_dictionary_differs {
                out.push_str(&format!("- `{sample}`\n"));
            }
        }
        out
    }
}

fn bucket_mut<'a>(
    map: &'a mut BTreeMap<String, SwitchTelemetryCounts>,
    key: &str,
    overflow_key: &str,
) -> &'a mut SwitchTelemetryCounts {
    let target = if map.contains_key(key) || map.len() < MAX_SWITCH_TELEMETRY_BUCKETS {
        key
    } else {
        overflow_key
    };
    map.entry(target.to_string()).or_default()
}

fn call_site_bucket_mut<'a>(
    map: &'a mut BTreeMap<String, SwitchCallSiteCounts>,
    call_site: &str,
) -> &'a mut SwitchCallSiteCounts {
    let target = if map.contains_key(call_site) || map.len() < MAX_CALL_SITE_TELEMETRY_BUCKETS {
        call_site
    } else {
        OTHER_KEY_BUCKET
    };
    map.entry(target.to_string()).or_default()
}

#[derive(Debug, Default)]
pub(super) struct SwitchTelemetryState {
    emitted: u64,
    replaced: u64,
    superseded: u64,
    new: u64,
    non_first_set_emitted: u64,
    helper_without_token: u64,
    helper_without_token_with_projected_event: u64,
    helper_without_token_no_projected_event: u64,
    dictionary_same: u64,
    dictionary_differs: u64,
    evictions: u64,
    by_scope: ItemCompareByScope<SwitchScopeOutcomeCounts>,
    by_class: BTreeMap<String, SwitchTelemetryCounts>,
    by_dispatch_method: BTreeMap<String, SwitchTelemetryCounts>,
    by_helper: BTreeMap<String, SwitchTelemetryCounts>,
    by_class_rpc_helper: BTreeMap<String, SwitchTelemetryCounts>,
    by_call_site: BTreeMap<String, SwitchCallSiteCounts>,
    not_covered_by_reason: BTreeMap<String, u64>,
    not_covered_by_call_site_reason: BTreeMap<String, BTreeMap<String, u64>>,
    hand_sent_by_call_site: BTreeMap<String, u64>,
    sample_hand_sent: Vec<String>,
    projected_by_rule: BTreeMap<String, u64>,
    projected_by_rule_rpc: BTreeMap<String, BTreeMap<String, u64>>,
    sample_new: Vec<String>,
    sample_superseded: Vec<String>,
    sample_helper_without_token: Vec<String>,
    sample_helper_without_token_with_projected_event: Vec<String>,
    sample_helper_without_token_no_projected_event: Vec<String>,
    sample_dictionary_differs: Vec<String>,
}

impl SwitchTelemetryState {
    fn scope_counts_mut(&mut self, scope: CompareOriginScope) -> &mut SwitchScopeOutcomeCounts {
        match scope {
            CompareOriginScope::Dispatch => &mut self.by_scope.dispatch,
            CompareOriginScope::Tick => &mut self.by_scope.tick,
            CompareOriginScope::Neither => &mut self.by_scope.neither,
        }
    }

    fn bump(
        &mut self,
        class_key: &str,
        rpc_method: &str,
        helper: Option<&str>,
        f: impl Fn(&mut SwitchTelemetryCounts),
    ) {
        let helper_str = helper.unwrap_or("<none>");
        f(bucket_mut(&mut self.by_class, class_key, OTHER_CLASS_BUCKET));
        f(bucket_mut(
            &mut self.by_dispatch_method,
            rpc_method,
            OTHER_RPC_BUCKET,
        ));
        if let Some(h) = helper {
            f(bucket_mut(&mut self.by_helper, h, OTHER_KEY_BUCKET));
        }
        let compound = format!("class={class_key}|rpc={rpc_method}|helper={helper_str}");
        f(bucket_mut(
            &mut self.by_class_rpc_helper,
            &compound,
            OTHER_KEY_BUCKET,
        ));
    }

    pub(super) fn record_emitted(
        &mut self,
        origin_scope: CompareOriginScope,
        emit_live: bool,
        class_key: &str,
        rpc_method: &str,
        recipient_rule: &str,
    ) {
        self.scope_counts_mut(origin_scope).would_announce += 1;
        if emit_live {
            self.emitted += 1;
            self.bump(class_key, rpc_method, None, |c| c.emitted += 1);
            if !recipient_rule.is_empty() {
                *self
                    .projected_by_rule
                    .entry(recipient_rule.to_string())
                    .or_insert(0) += 1;
                let rule_rpcs = self
                    .projected_by_rule_rpc
                    .entry(recipient_rule.to_string())
                    .or_default();
                let rpc_key = if rule_rpcs.contains_key(rpc_method)
                    || rule_rpcs.len() < MAX_SWITCH_TELEMETRY_BUCKETS
                {
                    rpc_method
                } else {
                    OTHER_RPC_BUCKET
                };
                *rule_rpcs.entry(rpc_key.to_string()).or_insert(0) += 1;
            }
        }
    }

    pub(super) fn record_hand_sent(
        &mut self,
        call_site: &str,
        rpc_method: &str,
        origin_scope: CompareOriginScope,
        class_key: &str,
        holder_kind: &str,
        is_actor: bool,
    ) {
        let target_site = if self.hand_sent_by_call_site.contains_key(call_site)
            || self.hand_sent_by_call_site.len() < MAX_CALL_SITE_TELEMETRY_BUCKETS
        {
            call_site
        } else {
            OTHER_KEY_BUCKET
        };
        *self
            .hand_sent_by_call_site
            .entry(target_site.to_string())
            .or_insert(0) += 1;
        if self.sample_hand_sent.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
            let origin_str = match origin_scope {
                CompareOriginScope::Dispatch => "dispatch",
                CompareOriginScope::Tick => "tick",
                CompareOriginScope::Neither => "outside",
            };
            let actor_flag = u8::from(is_actor);
            self.sample_hand_sent.push(format!(
                "site={call_site} rpc={rpc_method} origin={origin_str} class={class_key} holder={holder_kind} actor={actor_flag}"
            ));
        }
    }

    #[inline]
    pub(super) fn record_shadow_outcome(
        &mut self,
        origin_scope: CompareOriginScope,
        is_replaced: bool,
    ) {
        let sc = self.scope_counts_mut(origin_scope);
        if is_replaced {
            sc.replaced += 1;
        } else {
            sc.superseded += 1;
        }
    }

    #[inline]
    pub(super) fn record_shadow_no_projected_event(&mut self, origin_scope: CompareOriginScope) {
        self.scope_counts_mut(origin_scope).no_projected_event += 1;
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_replaced(
        &mut self,
        origin_scope: CompareOriginScope,
        count_live_replacement: bool,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        call_site: &str,
        item_id: i64,
        expected_dict: Option<&CanonicalChangeDict>,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        self.scope_counts_mut(origin_scope).replaced += 1;
        if !count_live_replacement {
            return;
        }
        self.replaced += 1;
        call_site_bucket_mut(&mut self.by_call_site, call_site).replaced += 1;
        let same_dict = matches!((expected_dict, actual_dict), (Some(a), Some(b)) if a == b);
        if same_dict {
            self.dictionary_same += 1;
        } else {
            self.dictionary_differs += 1;
            if self.sample_dictionary_differs.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
                let exp_s = expected_dict
                    .map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                let act_s = actual_dict
                    .map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                self.sample_dictionary_differs.push(format!(
                    "item={item_id} first_set=true class={class_key} rpc={rpc_method} helper={helper} site={call_site} projected_dict={exp_s} actual_dict={act_s}"
                ));
            }
        }
        self.bump(class_key, rpc_method, Some(helper), |c| {
            c.replaced += 1;
            if same_dict {
                c.dictionary_same += 1;
            } else {
                c.dictionary_differs += 1;
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_superseded(
        &mut self,
        origin_scope: CompareOriginScope,
        count_live: bool,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        call_site: &str,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        expected_fp: ItemWireFingerprint,
        actual_fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        self.scope_counts_mut(origin_scope).superseded += 1;
        if !count_live {
            return;
        }
        self.superseded += 1;
        call_site_bucket_mut(&mut self.by_call_site, call_site).superseded += 1;
        if self.sample_superseded.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
            let act_s = actual_dict
                .map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
            self.sample_superseded.push(format!(
                "cid={char_id} item={item_id} kind={kind:?} class={class_key} rpc={rpc_method} helper={helper} site={call_site} fact=({},{},{}) helper_row=({},{},{}) dict={act_s}",
                expected_fp.location_id,
                expected_fp.flag_id,
                expected_fp.wire_qty,
                actual_fp.location_id,
                actual_fp.flag_id,
                actual_fp.wire_qty,
            ));
        }
        self.bump(class_key, rpc_method, Some(helper), |c| c.superseded += 1);
    }

    pub(super) fn record_non_first_set_emitted(&mut self, call_site: &str) {
        self.non_first_set_emitted += 1;
        call_site_bucket_mut(&mut self.by_call_site, call_site).non_first_set_emitted += 1;
    }

    pub(super) fn record_not_covered(&mut self, call_site: &str, reason: NotCoveredReason) {
        let key = reason.as_str();
        *self.not_covered_by_reason.entry(key.to_string()).or_insert(0) += 1;
        *self
            .not_covered_by_call_site_reason
            .entry(call_site.to_string())
            .or_default()
            .entry(key.to_string())
            .or_insert(0) += 1;
    }

    pub(super) fn record_new(&mut self, item_id: i64, entry: &ProjectedDedupeEntry) {
        self.new += 1;
        if self.sample_new.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
            self.sample_new.push(format!(
                "item={item_id} kind={:?} class={} rpc={}",
                entry.kind, entry.class_key, entry.rpc_method
            ));
        }
        self.bump(&entry.class_key, &entry.rpc_method, None, |c| c.new += 1);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_helper_without_token(
        &mut self,
        origin_scope: CompareOriginScope,
        count_live: bool,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        call_site: &str,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        self.scope_counts_mut(origin_scope).no_projected_event += 1;
        if !count_live {
            return;
        }
        self.helper_without_token += 1;
        let site_counts = call_site_bucket_mut(&mut self.by_call_site, call_site);
        self.helper_without_token_no_projected_event += 1;
        site_counts.helper_without_token_no_projected_event += 1;
        let need_total_sample =
            self.sample_helper_without_token.len() < MAX_SWITCH_TELEMETRY_SAMPLES;
        let need_split_sample =
            self.sample_helper_without_token_no_projected_event.len() < MAX_SWITCH_TELEMETRY_SAMPLES;
        if need_total_sample || need_split_sample {
            let act_s = actual_dict
                .map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
            let sample = format!(
                "cid={char_id} item={item_id} kind={kind:?} had_projected_event=false class={class_key} rpc={rpc_method} helper={helper} site={call_site} dict={act_s}"
            );
            if need_total_sample {
                self.sample_helper_without_token.push(sample.clone());
            }
            if need_split_sample {
                self.sample_helper_without_token_no_projected_event
                    .push(sample);
            }
        }
        self.bump(class_key, rpc_method, Some(helper), |c| {
            c.helper_without_token += 1;
            c.helper_without_token_no_projected_event += 1;
        });
    }

    pub(super) fn to_summary(&self, config: ProjectedItemSwitchConfig) -> ProjectedItemSwitchSummary {
        ProjectedItemSwitchSummary {
            config,
            emitted: self.emitted,
            replaced: self.replaced,
            superseded: self.superseded,
            new: self.new,
            non_first_set_emitted: self.non_first_set_emitted,
            helper_without_token: self.helper_without_token,
            helper_without_token_with_projected_event: self
                .helper_without_token_with_projected_event,
            helper_without_token_no_projected_event: self.helper_without_token_no_projected_event,
            dictionary_same: self.dictionary_same,
            dictionary_differs: self.dictionary_differs,
            evictions: self.evictions,
            by_scope: self.by_scope.clone(),
            by_class: self.by_class.clone(),
            by_dispatch_method: self.by_dispatch_method.clone(),
            by_helper: self.by_helper.clone(),
            by_class_rpc_helper: self.by_class_rpc_helper.clone(),
            by_call_site: self.by_call_site.clone(),
            not_covered_by_reason: self.not_covered_by_reason.clone(),
            not_covered_by_call_site_reason: self.not_covered_by_call_site_reason.clone(),
            hand_sent_by_call_site: self.hand_sent_by_call_site.clone(),
            sample_hand_sent: self.sample_hand_sent.clone(),
            projected_by_rule: self.projected_by_rule.clone(),
            projected_by_rule_rpc: self.projected_by_rule_rpc.clone(),
            account_switches: AccountSwitchSummary::default(),
            sample_new: self.sample_new.clone(),
            sample_superseded: self.sample_superseded.clone(),
            sample_helper_without_token: self.sample_helper_without_token.clone(),
            sample_helper_without_token_with_projected_event: self
                .sample_helper_without_token_with_projected_event
                .clone(),
            sample_helper_without_token_no_projected_event: self
                .sample_helper_without_token_no_projected_event
                .clone(),
            sample_dictionary_differs: self.sample_dictionary_differs.clone(),
        }
    }
}
