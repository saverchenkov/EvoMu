//! `browser_lockdown` — In-game browser lockdown policy service (`browserLockdownSvc`).
//!
//! Implements V24 `carbonui/control/browser/sites.pyj` RPCs:
//! - `browserLockdownSvc.IsBrowserInLockdown`
//! - `browserLockdownSvc.GetDefaultHomePage`
//! - `browserLockdownSvc.GetFlaggedSitesList`
//! - `browserLockdownSvc.GetFlaggedSitesHash`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};

use crate::router::ServerContext;

/// Blob key in `WorldStateSnapshot.service_state_blobs` for browser lockdown policy.
pub const BROWSER_LOCKDOWN_POLICY_KEY: &str = "browser_lockdown_policy";

/// Default EVE Online home page returned when no custom home page is configured.
pub const DEFAULT_HOME_PAGE: &str = "https://www.eveonline.com";

/// Site flag bit for an untrusted / phishing / malicious URL (`sites.pyj`).
pub const SITE_FLAG_UNTRUSTED: i64 = 1;

/// Site flag bit for an official CCP / trusted URL (`sites.pyj`).
pub const SITE_FLAG_TRUSTED_CCP: i64 = 2;

/// Single flagged site record matching V24 `SitesSvc.__PopulateSystemLists` (`siteUrl`, `siteFlag`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlaggedSiteEntry {
    pub site_url: String,
    pub site_flag: i64,
}

/// Persisted in-game browser lockdown policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserLockdownPolicy {
    pub in_lockdown: bool,
    pub default_home_page: String,
    pub flagged_sites: Vec<FlaggedSiteEntry>,
}

impl Default for BrowserLockdownPolicy {
    fn default() -> Self {
        Self {
            in_lockdown: false,
            default_home_page: DEFAULT_HOME_PAGE.to_string(),
            flagged_sites: Vec::new(),
        }
    }
}

/// Load the browser lockdown policy from `WorldStateSnapshot.service_state_blobs`.
pub fn load_browser_lockdown_policy(snap: &eve_db::WorldStateSnapshot) -> BrowserLockdownPolicy {
    snap.service_state_blobs
        .get(BROWSER_LOCKDOWN_POLICY_KEY)
        .and_then(|json| serde_json::from_str::<BrowserLockdownPolicy>(json).ok())
        .unwrap_or_default()
}

/// Save the browser lockdown policy into `WorldStateSnapshot.service_state_blobs`.
pub fn save_browser_lockdown_policy(
    snap: &mut eve_db::WorldStateSnapshot,
    policy: &BrowserLockdownPolicy,
) {
    if let Ok(json) = serde_json::to_string(policy) {
        snap.service_state_blobs
            .insert(BROWSER_LOCKDOWN_POLICY_KEY.to_string(), json);
    }
}

/// Compute a deterministic 32-character hex digest for `(site_url, site_flag)` entries.
pub fn compute_flagged_sites_hash(flagged_sites: &[FlaggedSiteEntry]) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for entry in flagged_sites {
        for &b in entry.site_url.as_bytes() {
            h1 ^= u64::from(b);
            h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
            h2 = h2.wrapping_add(u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        for b in entry.site_flag.to_le_bytes() {
            h1 ^= u64::from(b);
            h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
            h2 = h2.wrapping_add(u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        h1 ^= 0x0a;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 = h2.wrapping_add(0x0a).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    format!("{h1:016x}{h2:016x}")
}

/// Dispatches `browserLockdownSvc` RPC calls (`feat:ui.browser-lockdown`).
// parity: feat:ui.browser-lockdown
// parity: rpc:browserLockdownSvc.IsBrowserInLockdown
// parity: rpc:browserLockdownSvc.GetDefaultHomePage
// parity: rpc:browserLockdownSvc.GetFlaggedSitesList
// parity: rpc:browserLockdownSvc.GetFlaggedSitesHash
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "browserLockdownSvc" {
        return None;
    }

    match method {
        "IsBrowserInLockdown" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let in_lockdown = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).in_lockdown);
            Some(Ok(PyRep::Bool(in_lockdown)))
        }
        "GetDefaultHomePage" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let home = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).default_home_page);
            Some(Ok(PyRep::String(home)))
        }
        "GetFlaggedSitesList" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let sites = ctx
                .db
                .with_snapshot(|snap| load_browser_lockdown_policy(snap).flagged_sites);
            let entries = sites
                .into_iter()
                .map(|entry| {
                    PyRep::Dict(vec![
                        (PyRep::String("siteUrl".into()), PyRep::String(entry.site_url)),
                        (PyRep::String("siteFlag".into()), PyRep::Integer(entry.site_flag)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(entries)))
        }
        "GetFlaggedSitesHash" => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let hash = ctx.db.with_snapshot(|snap| {
                let policy = load_browser_lockdown_policy(snap);
                compute_flagged_sites_hash(&policy.flagged_sites)
            });
            Some(Ok(PyRep::String(hash)))
        }
        _ => None,
    }
}
