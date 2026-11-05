//! `social_corp_projects` — Canonical Corporation Projects & Freelance Ledger (`SO-3`, `Q-4`, `Q-5`).
//!
//! Unifies V24.01 Quasar corporation goals (`eve_public.goal.*`, `eve_public.corporationgoal.*`,
//! `eve.corporationgoal.*`) and freelance jobs (`eve_public.freelance.*`) onto the canonical
//! `snap.corp_projects` + `snap.alliance_war_state.{project_meta, project_details}` store, with
//! server-observed gameplay hooks and full `wallet_journal` + `corp_wallet_journals` accounting.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use crate::router::ServerContext;
use crate::services::quasar_goal_tale_notices as notices;
use eve_db::{
    corp_roles, CorpProjectRecord, IskCents, PersistedProjectDetails, WalletJournalEntry,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};

pub const SERVER_TRACKED_MATCHERS: &[&str] = &[
    "DamageNPCShips",
    "DestroyNPCShips",
    "DestroyCapsuleerShips",
    "MineMaterials",
    "MineOre",
    "ManufactureItems",
    "CaptureFWComplexes",
    "DefendFWComplexes",
    "DamageShips",
    "RemoteRepairShield",
    "RemoteRepairArmor",
    "ScanSignatures",
    "SalvageWrecks",
    "EarnLoyaltyPoints",
];

/// Delayed 24h division access/take role bits: hangar divisions (`13..=26`) + `ROLE_ACCOUNT_CAN_TAKE` (`27..=33`).
pub(crate) const HANGAR_DIV_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;

pub fn corp_goal_uuid(project_id: i64) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(notices::CORP_GOAL_UUID_PREFIX);
    uuid[8..16].copy_from_slice(&(project_id as u64).to_be_bytes());
    uuid
}

pub fn goal_uuid_to_project_id(uuid: &[u8]) -> Option<i64> {
    if uuid.len() == 16 && &uuid[0..8] == notices::CORP_GOAL_UUID_PREFIX {
        let raw = u64::from_be_bytes(uuid[8..16].try_into().ok()?);
        i64::try_from(raw).ok().filter(|&id| id > 0)
    } else {
        None
    }
}

pub fn freelance_uuid(created_unix: u64, project_id: i64) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    let prefix = if &created_unix.to_be_bytes() == notices::CORP_GOAL_UUID_PREFIX {
        created_unix.wrapping_add(1)
    } else {
        created_unix
    };
    uuid[0..8].copy_from_slice(&prefix.to_be_bytes());
    uuid[8..16].copy_from_slice(&(project_id as u64).to_be_bytes());
    uuid
}

pub fn freelance_uuid_to_project_id(uuid: &[u8]) -> Option<i64> {
    if uuid.len() == 16 && &uuid[0..8] != notices::CORP_GOAL_UUID_PREFIX {
        let raw = u64::from_be_bytes(uuid[8..16].try_into().ok()?);
        i64::try_from(raw).ok().filter(|&id| id > 0)
    } else {
        None
    }
}

pub(crate) fn sync_corp_project_contributors(snap: &mut eve_db::WorldStateSnapshot, pid: i64) {
    let Some(det) = snap.alliance_war_state.project_details.get(&pid) else {
        return;
    };
    let mut entries: Vec<eve_db::snapshot_ext::CorpProjectContributorEntry> = det
        .char_contributions
        .iter()
        .filter(|(_, &q)| q > 0)
        .map(|(&cid, &q)| eve_db::snapshot_ext::CorpProjectContributorEntry {
            character_id: cid,
            contributed_quantity: q,
        })
        .collect();
    entries.sort_by_key(|c| c.character_id);
    snap.corp_project_contributors.insert(pid, entries);
}

fn append_corp_journal(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    div_id: u8,
    now_unix: u64,
    ref_type: &str,
    description: String,
    signed_amount: IskCents,
    balance_after: IskCents,
    second_party_id: Option<i64>,
    reason_label: &str,
) {
    snap.next_entity_id += 1;
    let jid = snap.next_entity_id;
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: jid,
        character_id: corp_id,
        entry_type: ref_type.into(),
        amount: signed_amount,
        balance_after,
        reason: description.clone(),
    });
    snap.wallet_journal_timestamps.insert(jid, now_unix);
    let next_cjid = snap
        .corp_wallet_journals
        .values()
        .flatten()
        .map(|j| j.id)
        .max()
        .unwrap_or(500_000)
        + 1;
    snap.corp_wallet_journals
        .entry((corp_id, div_id.clamp(1, 7) as i64))
        .or_default()
        .push(eve_db::snapshot_ext::CorpWalletJournalRecord {
            id: next_cjid,
            date_unix: now_unix,
            ref_type: ref_type.into(),
            description,
            amount: signed_amount.to_f64_isk(),
            balance: balance_after.to_f64_isk(),
            first_party_id: Some(corp_id),
            second_party_id,
            reason: Some(reason_label.into()),
        });
}

/// Computes `(is_ceo, effective_roles)` for `char_id` in `corp_id` from DB corporation state
/// (`corp.member_roles`, `corp_role_assignments`, and 1-based title bitmasks `1 << (tid - 1)`),
/// masking unmatured 24h `pending_role_grants` and `pending_title_role_grants` on `HANGAR_DIV_MASK`.
pub(crate) fn effective_corp_member_roles(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    session_roles: u64,
    now_unix: u64,
) -> (bool, u64) {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return (false, session_roles);
    };
    let is_ceo = corp.ceo_id == char_id;
    let mut roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
    if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
        roles |= ra.roles;
    }
    if let Some(gov) = snap.corp_governance.get(&corp_id) {
        if let Some(&(p_mask, matured_at)) = gov.pending_role_grants.get(&char_id) {
            if now_unix >= matured_at {
                roles |= p_mask;
            }
        }
        let unmatured_title_bits = gov
            .pending_title_role_grants
            .get(&char_id)
            .filter(|&&(_, matured_at)| now_unix < matured_at)
            .map_or(0, |&(bits, _)| bits & HANGAR_DIV_MASK);
        if let Some(&tmask) = gov.member_titles.get(&char_id) {
            for (&tid, &(_, troles)) in &gov.titles {
                if (1..=64).contains(&tid) && (tmask & (1u64 << (tid - 1))) != 0 {
                    roles |= troles & !unmatured_title_bits;
                }
            }
        }
    }
    (is_ceo, roles)
}

fn extract_first_positive_id_in_msg(msg: &DynamicQuasarMessage) -> Option<i64> {
    let mut keys: Vec<u32> = msg.fields.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        let Some(v) = msg.get(k) else { continue };
        match v {
            DynamicValue::Uint64(u) if *u > 0 => return i64::try_from(*u).ok(),
            DynamicValue::Int64(i) | DynamicValue::Sint64(i) if *i > 0 => return Some(*i),
            DynamicValue::Uint32(u) if *u > 0 => return Some(*u as i64),
            DynamicValue::Int32(i) | DynamicValue::Sint32(i) if *i > 0 => return Some(*i as i64),
            DynamicValue::Message(sub) => {
                if let Some(id) = extract_first_positive_id_in_msg(sub) {
                    return Some(id);
                }
            }
            DynamicValue::Repeated(items) => {
                for item in items {
                    match item {
                        DynamicValue::Message(sub) => {
                            if let Some(id) = extract_first_positive_id_in_msg(sub) {
                                return Some(id);
                            }
                        }
                        DynamicValue::Uint64(u) if *u > 0 => return i64::try_from(*u).ok(),
                        DynamicValue::Int64(i) | DynamicValue::Sint64(i) if *i > 0 => return Some(*i),
                        DynamicValue::Uint32(u) if *u > 0 => return Some(*u as i64),
                        DynamicValue::Int32(i) | DynamicValue::Sint32(i) if *i > 0 => return Some(*i as i64),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses V24 `eve_public.goal.ContributionConfiguration` (`field 4` of `eve_public.goal.api.CreateRequest`)
/// into canonical `(matcher_type, filter_id)`. Omitting `field 4` returns `("Any", None)`.
pub fn parse_goal_contribution_config(val: Option<&DynamicValue>) -> (String, Option<i64>) {
    let Some(DynamicValue::Message(cfg)) = val else {
        return ("Any".into(), None);
    };
    let mappings: &[(u32, &str)] = &[
        (1, "Manual"),
        (2, "DeliverItem"),
        (37, "DeliverItem"),
        (3, "DestroyNPCShips"),
        (31, "DestroyNPCShips"),
        (4, "DamageShips"),
        (26, "DamageShips"),
        (5, "MineMaterials"),
        (22, "MineMaterials"),
        (25, "MineMaterials"),
        (6, "DestroyCapsuleerShips"),
        (27, "DestroyCapsuleerShips"),
        (7, "CaptureFWComplexes"),
        (34, "CaptureFWComplexes"),
        (8, "DefendFWComplexes"),
        (35, "DefendFWComplexes"),
        (9, "RemoteRepairArmor"),
        (32, "RemoteRepairArmor"),
        (10, "RemoteRepairShield"),
        (33, "RemoteRepairShield"),
        (11, "ManufactureItems"),
        (13, "ManufactureItems"),
        (14, "ManufactureItems"),
        (38, "ManufactureItems"),
        (12, "ScanSignatures"),
        (28, "ScanSignatures"),
        (16, "SalvageWrecks"),
        (36, "SalvageWrecks"),
        (17, "EarnLoyaltyPoints"),
        (19, "EarnLoyaltyPoints"),
        (29, "EarnLoyaltyPoints"),
    ];
    for &(field_num, matcher) in mappings {
        if let Some(sub_val) = cfg.get(field_num) {
            let filter_id = match sub_val {
                DynamicValue::Message(m) if matcher != "Manual" => extract_first_positive_id_in_msg(m),
                _ => None,
            };
            return (matcher.into(), filter_id);
        }
    }
    ("Any".into(), None)
}

pub struct ParsedGoalCreateOptions {
    pub matcher_type: String,
    pub filter_id: Option<i64>,
    pub escrow: IskCents,
    pub isk_per_unit: IskCents,
    pub auto_payout: bool,
    pub expires_unix: Option<u64>,
    pub max_per_character: Option<u64>,
}

fn parse_isk_value(val: &DynamicValue) -> Result<i64, (u32, String)> {
    match val {
        DynamicValue::Sint64(v) | DynamicValue::Int64(v) => {
            (*v >= 0).then_some(*v).ok_or_else(|| (STATUS_BAD_REQUEST, "Negative goal reward amount".into()))
        }
        DynamicValue::Uint64(u) => i64::try_from(*u).map_err(|_| (STATUS_BAD_REQUEST, "Goal reward overflow".into())),
        DynamicValue::Sint32(v) | DynamicValue::Int32(v) => {
            (*v >= 0).then_some(*v as i64).ok_or_else(|| (STATUS_BAD_REQUEST, "Negative goal reward amount".into()))
        }
        DynamicValue::Uint32(u) => Ok(*u as i64),
        DynamicValue::Message(m) => {
            if let Some(sub2) = m.get(2) {
                if matches!(sub2, DynamicValue::Message(_)) {
                    return parse_isk_value(sub2);
                }
                if let Some(v) = parse_isk_value(sub2).ok().filter(|&v| v > 0) {
                    return Ok(v);
                }
            }
            m.get(1).map_or(Ok(0), parse_isk_value)
        }
        DynamicValue::Repeated(list) => list.iter().try_fold(0i64, |sum, item| {
            sum.checked_add(parse_isk_value(item)?).ok_or_else(|| (STATUS_BAD_REQUEST, "Goal reward overflow".into()))
        }),
        _ => Ok(0),
    }
}

fn has_contribution_period(val: Option<&DynamicValue>) -> bool {
    match val {
        Some(DynamicValue::Message(m)) => {
            matches!(m.get(2), Some(DynamicValue::Message(_)))
                && matches!(
                    m.get(1),
                    Some(
                        DynamicValue::Enum(1)
                            | DynamicValue::Uint64(1)
                            | DynamicValue::Int64(1)
                            | DynamicValue::Uint32(1)
                            | DynamicValue::Int32(1)
                    )
                )
        }
        Some(DynamicValue::Repeated(list)) => list.iter().any(|x| has_contribution_period(Some(x))),
        _ => false,
    }
}

pub fn parse_goal_create_options(
    dyn_msg: &DynamicQuasarMessage,
    target: u64,
) -> Result<ParsedGoalCreateOptions, (u32, String)> {
    let (matcher_type, filter_id) = parse_goal_contribution_config(dyn_msg.get(4));
    let mut escrow_isk = dyn_msg.get(8).map_or(Ok(0i64), parse_isk_value)?;
    let explicit_unit = dyn_msg.get(7).map_or(Ok(0i64), parse_isk_value)?;
    if explicit_unit > 0 && escrow_isk == 0 {
        escrow_isk = explicit_unit.saturating_mul(target as i64);
    }
    let auto_payout = has_contribution_period(dyn_msg.get(7)) || has_contribution_period(dyn_msg.get(8));
    let no_expiry = matches!(dyn_msg.get(9), Some(DynamicValue::Bool(true) | DynamicValue::Uint64(1) | DynamicValue::Int64(1)));
    let expires_unix = (!no_expiry).then(|| {
        dyn_msg.get(10).and_then(|v| match v {
            DynamicValue::Message(m) => m.get(1).and_then(|s| match s {
                DynamicValue::Uint64(u) if *u > 0 => Some(*u),
                DynamicValue::Int64(i) | DynamicValue::Sint64(i) if *i > 0 => Some(*i as u64),
                _ => None,
            }),
            _ => None,
        })
    }).flatten();
    let unlimited = matches!(
        dyn_msg.get(11).or_else(|| dyn_msg.get(25)),
        Some(DynamicValue::Bool(true) | DynamicValue::Uint64(1) | DynamicValue::Int64(1))
    );
    let max_per_character = (!unlimited).then(|| {
        dyn_msg.get(26).or_else(|| dyn_msg.get(12)).and_then(|v| match v {
            DynamicValue::Uint64(u) if *u > 0 => Some(*u),
            DynamicValue::Int64(i) | DynamicValue::Sint64(i) if *i > 0 => Some(*i as u64),
            _ => None,
        })
    }).flatten();
    let isk_per_unit = if explicit_unit > 0 {
        IskCents::from_isk(explicit_unit)
    } else if auto_payout && escrow_isk > 0 && target > 0 {
        IskCents::from_isk(escrow_isk / (target as i64))
    } else {
        IskCents::ZERO
    };
    Ok(ParsedGoalCreateOptions {
        matcher_type,
        filter_id,
        escrow: IskCents::from_isk(escrow_isk),
        isk_per_unit,
        auto_payout,
        expires_unix,
        max_per_character,
    })
}

pub struct CanonicalProjectSpec {
    pub corp_id: i64,
    pub creator_id: i64,
    pub name: String,
    pub description: String,
    pub target_quantity: u64,
    pub initial_quantity: u64,
    pub escrow: IskCents,
    pub isk_per_unit: IskCents,
    pub wallet_division: u8,
    pub matcher_type: String,
    pub is_freelance: bool,
    pub auto_payout_on_contribution: bool,
    pub max_per_character: Option<u64>,
    pub expires_unix: Option<u64>,
    pub filter_id: Option<i64>,
    pub batch_size: u64,
    pub now_unix: u64,
    pub min_project_id: i64,
}

pub fn create_canonical_project(
    db: &WorldRepository,
    spec: CanonicalProjectSpec,
) -> Result<(i64, [u8; 16]), (u32, String)> {
    if spec.target_quantity == 0 || spec.escrow.raw_cents() < 0 || spec.isk_per_unit.raw_cents() < 0 {
        return Err((STATUS_BAD_REQUEST, "Invalid project target or escrow amount".into()));
    }
    let div = spec.wallet_division.clamp(1, 7);
    db.try_with_mut_snapshot(|snap| {
        let _ = sweep_expired_corp_projects_in_snap(snap, spec.now_unix);
        if spec.is_freelance {
            let active_fl_count = snap
                .corp_projects
                .values()
                .filter(|p| {
                    p.corporation_id == spec.corp_id
                        && snap.alliance_war_state.project_meta.get(&p.project_id).is_some_and(|m| m.1)
                        && snap
                            .alliance_war_state
                            .project_details
                            .get(&p.project_id)
                            .map_or(true, |d| d.status == "Active")
                })
                .count();
            if active_fl_count >= 100 {
                return Err((
                    STATUS_BAD_REQUEST,
                    "MaxFreelanceProjectsReached: Corporation cannot have more than 100 active freelance jobs".into(),
                ));
            }
        }

        if spec.escrow.raw_cents() > 0 {
            let Some(corp) = snap.corporations.get_mut(&spec.corp_id) else {
                return Err((STATUS_BAD_REQUEST, "Corporation not found for project escrow".into()));
            };
            let cur = if div == 1 && corp.master_wallet.raw_cents() > 0 {
                corp.master_wallet
            } else {
                corp.ensure_wallet_divisions();
                corp.division_balance(div)
            };
            let Some(rem) = cur.checked_sub(spec.escrow) else {
                return Err((
                    STATUS_BAD_REQUEST,
                    format!("CorpInsufficientFunds: Insufficient ISK in corporation wallet division {div} for project escrow"),
                ));
            };
            corp.set_division_balance(div, rem);
            append_corp_journal(
                snap,
                spec.corp_id,
                div,
                spec.now_unix,
                "CorporationProjectEscrow",
                format!("Project escrow reserved for project '{}'", spec.name),
                IskCents(-spec.escrow.raw_cents()),
                rem,
                (spec.creator_id > 0).then_some(spec.creator_id),
                "Corporation project escrow",
            );
        }

        let max_existing = snap.corp_projects.keys().copied().max().unwrap_or(100);
        let pid = snap
            .next_entity_id
            .saturating_add(1)
            .max(max_existing.saturating_add(1))
            .max(spec.min_project_id.max(101));
        snap.next_entity_id = snap.next_entity_id.max(pid);

        let uuid = if spec.is_freelance {
            freelance_uuid(spec.now_unix, pid)
        } else {
            corp_goal_uuid(pid)
        };
        let init_q = spec.initial_quantity.min(spec.target_quantity);
        let done = init_q >= spec.target_quantity;
        snap.corp_projects.insert(
            pid,
            CorpProjectRecord {
                project_id: pid,
                corporation_id: spec.corp_id,
                name: spec.name,
                target_quantity: spec.target_quantity,
                current_quantity: init_q,
                isk_per_unit: spec.isk_per_unit,
                escrow_remaining: spec.escrow,
            },
        );
        snap.alliance_war_state
            .project_meta
            .insert(pid, (spec.matcher_type, spec.is_freelance, done));
        snap.alliance_war_state.project_details.insert(
            pid,
            PersistedProjectDetails {
                wallet_division: div,
                max_per_character: spec.max_per_character,
                expires_unix: spec.expires_unix,
                status: if done { "Completed".into() } else { "Active".into() },
                filter_id: spec.filter_id,
                char_contributions: Default::default(),
                contributions_by_char: Default::default(),
                description: spec.description,
                creator_id: spec.creator_id,
                created_unix: spec.now_unix,
                auto_payout_on_contribution: spec.auto_payout_on_contribution,
                batch_size: spec.batch_size.max(1),
                committed_participants: Vec::new(),
                uuid_bytes: uuid.to_vec(),
                initial_escrow: spec.escrow,
                ..Default::default()
            },
        );
        Ok((pid, uuid))
    })
}

pub fn close_or_delete_canonical_project(
    db: &WorldRepository,
    project_id: i64,
    is_delete: bool,
    now_unix: u64,
) -> Option<(i64, IskCents)> {
    db.with_mut_snapshot(|snap| {
        let corp_id = snap.corp_projects.get(&project_id)?.corporation_id;
        let div_id = snap
            .alliance_war_state
            .project_details
            .get(&project_id)
            .map(|d| d.wallet_division.clamp(1, 7))
            .unwrap_or(1);
        let refund = if is_delete {
            snap.alliance_war_state.project_details.remove(&project_id);
            snap.alliance_war_state.project_meta.remove(&project_id);
            snap.corp_project_contributors.remove(&project_id);
            snap.corp_projects
                .remove(&project_id)
                .map(|p| p.escrow_remaining)
                .unwrap_or(IskCents::ZERO)
        } else {
            if let Some(det) = snap.alliance_war_state.project_details.get_mut(&project_id) {
                det.status = "Closed".into();
                det.closed = true;
            }
            snap.corp_projects
                .get_mut(&project_id)
                .map(|p| std::mem::replace(&mut p.escrow_remaining, IskCents::ZERO))
                .unwrap_or(IskCents::ZERO)
        };
        if refund.raw_cents() > 0 {
            let mut bal_after = IskCents::ZERO;
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                let cur = if div_id == 1 && corp.master_wallet.raw_cents() > 0 {
                    corp.master_wallet
                } else {
                    corp.ensure_wallet_divisions();
                    corp.division_balance(div_id)
                };
                let next = cur.checked_add(refund).unwrap_or(cur);
                corp.set_division_balance(div_id, next);
                bal_after = next;
            }
            append_corp_journal(
                snap,
                corp_id,
                div_id,
                now_unix,
                "CorporationProjectRefund",
                format!("Project {project_id} escrow refund on closure"),
                refund,
                bal_after,
                None,
                "Corporation project escrow refund",
            );
        }
        Some((corp_id, refund))
    })
}

fn calc_corp_goal_unit_rate_cents(proj: &CorpProjectRecord, det: &PersistedProjectDetails) -> IskCents {
    if proj.isk_per_unit.raw_cents() > 0 {
        return proj.isk_per_unit;
    }
    let pool_isk = if det.initial_escrow.raw_cents() > 0 {
        det.initial_escrow.raw_cents() / 100
    } else {
        proj.escrow_remaining.raw_cents() / 100
    };
    let rate_isk = if proj.target_quantity > 0 && pool_isk > 0 {
        (pool_isk / (proj.target_quantity as i64)).clamp(1, 10_000)
    } else {
        10_000
    };
    IskCents::from_isk(rate_isk)
}

pub fn redeem_corp_goal_reward_for_char(
    db: &WorldRepository,
    project_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Result<i64, (u32, String)> {
    db.try_with_mut_snapshot(|snap| {
        let is_fl = snap.alliance_war_state.project_meta.get(&project_id).is_some_and(|m| m.1);
        let Some(proj) = snap.corp_projects.get(&project_id).cloned() else {
            return Err((STATUS_NOT_FOUND, "Corporation goal not found".into()));
        };
        if is_fl {
            return Err((STATUS_NOT_FOUND, "Corporation goal not found".into()));
        }
        let det_ro = snap
            .alliance_war_state
            .project_details
            .get(&project_id)
            .cloned()
            .unwrap_or_default();
        let total_prog = det_ro
            .char_contributions
            .get(&char_id)
            .copied()
            .ok_or_else(|| (STATUS_PRECONDITION_FAILED, "No contributor progress for corporation goal".into()))?;
        let redeemed_prog = det_ro.contributions_by_char.get(&char_id).copied().unwrap_or(0);
        let unredeemed = total_prog.saturating_sub(redeemed_prog);
        if unredeemed == 0 {
            return Err((STATUS_PRECONDITION_FAILED, "No unredeemed rewards available for corporation goal".into()));
        }
        let unit_rate = calc_corp_goal_unit_rate_cents(&proj, &det_ro);
        let calc_reward = unit_rate
            .checked_mul_qty(unredeemed as i64)
            .unwrap_or(IskCents::ZERO)
            .min(proj.escrow_remaining);
        if calc_reward.raw_cents() <= 0 {
            return Err((STATUS_PRECONDITION_FAILED, "Corporation goal reward escrow is empty".into()));
        }
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err((STATUS_PRECONDITION_FAILED, "Unable to credit corporation goal reward".into()));
        };
        let Some(new_bal) = ch.balance.checked_add(calc_reward) else {
            return Err((STATUS_PRECONDITION_FAILED, "Unable to credit corporation goal reward".into()));
        };
        ch.balance = new_bal;
        if let Some(p_mut) = snap.corp_projects.get_mut(&project_id) {
            p_mut.escrow_remaining = p_mut.escrow_remaining.checked_sub(calc_reward).unwrap_or(IskCents::ZERO);
        }
        if let Some(d_mut) = snap.alliance_war_state.project_details.get_mut(&project_id) {
            d_mut.contributions_by_char.insert(char_id, total_prog);
        }
        snap.append_journal(
            char_id,
            "CorporationGoalReward",
            calc_reward,
            new_bal,
            format!("Corporation goal {project_id} reward"),
        );
        let div_id = det_ro.wallet_division.clamp(1, 7);
        let corp_bal = snap
            .corporations
            .get(&proj.corporation_id)
            .map(|c| if div_id == 1 { c.master_wallet } else { c.division_balance(div_id) })
            .unwrap_or(IskCents::ZERO);
        append_corp_journal(
            snap,
            proj.corporation_id,
            div_id,
            now_unix,
            "CorporationProjectPayout",
            format!("Project {project_id} ({}) payout to pilot {char_id}", proj.name),
            IskCents(-calc_reward.raw_cents()),
            corp_bal,
            Some(char_id),
            "Corporation project contribution reward",
        );
        Ok((calc_reward.raw_cents() / 100).max(1))
    })
}

pub fn redeem_all_corp_goal_rewards_for_char(
    db: &WorldRepository,
    char_id: i64,
    now_unix: u64,
) -> Result<Vec<(i64, i64)>, (u32, String)> {
    let pids: Vec<i64> = db.with_snapshot(|snap| {
        let mut ids: Vec<i64> = snap
            .corp_projects
            .keys()
            .copied()
            .filter(|pid| !snap.alliance_war_state.project_meta.get(pid).is_some_and(|m| m.1))
            .collect();
        ids.sort_unstable();
        ids
    });
    let mut redeemed = Vec::new();
    for pid in pids {
        if let Ok(payout_isk) = redeem_corp_goal_reward_for_char(db, pid, char_id, now_unix) {
            redeemed.push((pid, payout_isk));
        }
    }
    Ok(redeemed)
}

pub fn redeem_freelance_reward_for_char(
    db: &WorldRepository,
    project_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Result<(u64, i64), (u32, String)> {
    db.try_with_mut_snapshot(|snap| {
        let is_fl = snap.alliance_war_state.project_meta.get(&project_id).is_some_and(|m| m.1);
        let Some(proj) = snap.corp_projects.get(&project_id).cloned() else {
            return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
        };
        if !is_fl {
            return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
        }
        let det_ro = snap
            .alliance_war_state
            .project_details
            .get(&project_id)
            .cloned()
            .unwrap_or_default();
        let total_prog = det_ro
            .char_contributions
            .get(&char_id)
            .copied()
            .ok_or_else(|| (STATUS_BAD_REQUEST, "No contributor progress for freelance project".into()))?;
        let batch = det_ro.batch_size.max(1);
        let total_batches = total_prog / batch;
        let redeemed_batches = det_ro.contributions_by_char.get(&char_id).copied().unwrap_or(0);
        let unredeemed = total_batches.saturating_sub(redeemed_batches);
        let batch_reward = if proj.isk_per_unit.raw_cents() > 0 {
            proj.isk_per_unit
                .checked_mul_qty(batch as i64)
                .unwrap_or(IskCents::ZERO)
        } else if proj.escrow_remaining.raw_cents() > 0 && proj.target_quantity > 0 {
            let unit_cents = (proj.escrow_remaining.raw_cents() / (proj.target_quantity as i64)).max(1);
            IskCents(unit_cents.saturating_mul(batch as i64))
        } else {
            IskCents::ZERO
        };
        let avail_batches = if batch_reward.raw_cents() > 0 {
            (proj.escrow_remaining.raw_cents().max(0) / batch_reward.raw_cents()) as u64
        } else {
            0
        };
        let units = unredeemed.min(avail_batches);
        if units == 0 {
            return Err((STATUS_BAD_REQUEST, "No redeemable freelance rewards available".into()));
        }
        let payout_cents = batch_reward
            .checked_mul_qty(units as i64)
            .unwrap_or(IskCents::ZERO)
            .min(proj.escrow_remaining);
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err((STATUS_BAD_REQUEST, "Unable to credit freelance reward".into()));
        };
        let Some(new_bal) = ch.balance.checked_add(payout_cents) else {
            return Err((STATUS_BAD_REQUEST, "Unable to credit freelance reward".into()));
        };
        ch.balance = new_bal;
        if let Some(p_mut) = snap.corp_projects.get_mut(&project_id) {
            p_mut.escrow_remaining = p_mut.escrow_remaining.checked_sub(payout_cents).unwrap_or(IskCents::ZERO);
        }
        if let Some(d_mut) = snap.alliance_war_state.project_details.get_mut(&project_id) {
            d_mut
                .contributions_by_char
                .insert(char_id, redeemed_batches.saturating_add(units));
        }
        snap.append_journal(
            char_id,
            "FreelanceReward",
            payout_cents,
            new_bal,
            "Freelance project reward",
        );
        let div_id = det_ro.wallet_division.clamp(1, 7);
        let corp_bal = snap
            .corporations
            .get(&proj.corporation_id)
            .map(|c| if div_id == 1 { c.master_wallet } else { c.division_balance(div_id) })
            .unwrap_or(IskCents::ZERO);
        append_corp_journal(
            snap,
            proj.corporation_id,
            div_id,
            now_unix,
            "CorporationProjectPayout",
            format!("Freelance project {project_id} ({}) payout to pilot {char_id}", proj.name),
            IskCents(-payout_cents.raw_cents()),
            corp_bal,
            Some(char_id),
            "Freelance project contribution reward",
        );
        Ok((units, payout_cents.raw_cents() / 100))
    })
}

pub fn sweep_expired_corp_projects_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
) -> Vec<(i64, i64)> {
    let mut expired_list = Vec::new();
    let expired_pids: Vec<i64> = snap
        .alliance_war_state
        .project_details
        .iter()
        .filter_map(|(&pid, det)| {
            (det.status == "Active" && det.expires_unix.is_some_and(|exp| now_unix >= exp))
                .then_some(pid)
        })
        .collect();
    for pid in expired_pids {
        let div_id = snap
            .alliance_war_state
            .project_details
            .get_mut(&pid)
            .map(|det| {
                det.status = "Expired".into();
                det.expired = true;
                det.wallet_division.clamp(1, 7)
            })
            .unwrap_or(1);
        if let Some(proj) = snap.corp_projects.get_mut(&pid) {
            let refund = std::mem::replace(&mut proj.escrow_remaining, IskCents::ZERO);
            let corp_id = proj.corporation_id;
            if refund.raw_cents() > 0 {
                let mut bal_after = IskCents::ZERO;
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    let cur = if div_id == 1 && corp.master_wallet.raw_cents() > 0 {
                        corp.master_wallet
                    } else {
                        corp.ensure_wallet_divisions();
                        corp.division_balance(div_id)
                    };
                    let next = cur.checked_add(refund).unwrap_or(cur);
                    corp.set_division_balance(div_id, next);
                    bal_after = next;
                }
                append_corp_journal(
                    snap,
                    corp_id,
                    div_id,
                    now_unix,
                    "CorporationProjectRefund",
                    format!("Project {pid} escrow refund on expiry"),
                    refund,
                    bal_after,
                    None,
                    "Corporation project escrow refund",
                );
            }
            expired_list.push((pid, corp_id));
        }
    }
    expired_list
}

fn emit_expired_quasar_notices(ctx: &ServerContext, expired: &[(i64, i64)], now_unix: u64) {
    if expired.is_empty() {
        return;
    }
    for &(pid, corp_id) in expired {
        if corp_id <= 0 {
            continue;
        }
        let (is_fl, uuid) = ctx.db.with_snapshot(|snap| {
            let fl = snap.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.1);
            let u = snap
                .alliance_war_state
                .project_details
                .get(&pid)
                .and_then(|d| <[u8; 16]>::try_from(d.uuid_bytes.as_slice()).ok())
                .unwrap_or_else(|| if fl { freelance_uuid(0, pid) } else { corp_goal_uuid(pid) });
            (fl, u)
        });
        let mut id_b = Vec::with_capacity(18);
        encode_bytes_field(1, &uuid, &mut id_b);
        let mut out = Vec::with_capacity(20);
        encode_bytes_field(1, &id_b, &mut out);
        let type_name = if is_fl {
            "eve_public.freelance.project.api.ExpiredNotice"
        } else {
            "eve_public.corporationgoal.api.ExpiredNotice"
        };
        let _ = ctx.quasar_gateway.publish_notice(
            NoticeScope::Corporation(corp_id as u32),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: out,
            },
            now_unix,
        );
    }
}

pub fn sweep_expired_corp_projects(ctx: &ServerContext) {
    let now_unix = ctx.now_unix();
    let expired = ctx
        .db
        .with_mut_snapshot(|snap| sweep_expired_corp_projects_in_snap(snap, now_unix));
    emit_expired_quasar_notices(ctx, &expired, now_unix);
}

struct ProjectActivityUpdate {
    pid: i64,
    corp_id: i64,
    creator_id: i64,
    is_freelance: bool,
    uuid: [u8; 16],
    prev_cur: u64,
    new_cur: u64,
    completed: bool,
    total_contributors: usize,
    auto_paid_isk: u64,
}

fn record_server_project_activity_detailed(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    matcher_types: &[&str],
    qty: u64,
    filter_opt: Option<i64>,
    now_unix: u64,
) -> (Vec<(i64, i64)>, Vec<ProjectActivityUpdate>) {
    if qty == 0 || char_id <= 0 {
        return (Vec::new(), Vec::new());
    }
    let expired = sweep_expired_corp_projects_in_snap(snap, now_unix);
    let char_corp_id = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let mut candidate_pids: Vec<i64> = snap
        .corp_projects
        .values()
        .filter_map(|p| {
            let (matcher, is_fl, _) = snap
                .alliance_war_state
                .project_meta
                .get(&p.project_id)
                .cloned()
                .unwrap_or_else(|| ("DeliverItem".into(), false, false));
            let det = snap
                .alliance_war_state
                .project_details
                .get(&p.project_id)
                .cloned()
                .unwrap_or_default();
            if det.status != "Active" || p.current_quantity >= p.target_quantity {
                return None;
            }
            if is_fl {
                if !det.committed_participants.contains(&char_id)
                    && !det.char_contributions.contains_key(&char_id)
                {
                    return None;
                }
            } else {
                let is_ceo_of_proj_corp = snap
                    .corporations
                    .get(&p.corporation_id)
                    .is_some_and(|c| c.ceo_id == char_id);
                if p.corporation_id != char_corp_id && !is_ceo_of_proj_corp {
                    return None;
                }
            }
            if matcher.eq_ignore_ascii_case("Manual") {
                return None;
            }
            if !matcher.eq_ignore_ascii_case("Any")
                && !matcher_types.iter().any(|m| m.eq_ignore_ascii_case(&matcher))
            {
                return None;
            }
            if let Some(req_f) = det.filter_id.filter(|&f| f > 0) {
                if filter_opt != Some(req_f) {
                    return None;
                }
            }
            Some(p.project_id)
        })
        .collect();
    candidate_pids.sort_unstable();

    let mut updates = Vec::new();
    for pid in candidate_pids {
        let Some(proj) = snap.corp_projects.get(&pid).cloned() else {
            continue;
        };
        let is_fl = snap
            .alliance_war_state
            .project_meta
            .get(&pid)
            .is_some_and(|m| m.1);
        let (accepted, auto_payout, div_id, creator_id, uuid, total_contributors) = {
            let det = snap
                .alliance_war_state
                .project_details
                .entry(pid)
                .or_default();
            let prev_char_qty = det.char_contributions.get(&char_id).copied().unwrap_or(0);
            let char_rem = det
                .max_per_character
                .map_or(u64::MAX, |cap| cap.saturating_sub(prev_char_qty));
            let proj_rem = proj.target_quantity.saturating_sub(proj.current_quantity);
            let acc = qty.min(proj_rem).min(char_rem);
            if acc == 0 {
                continue;
            }
            *det.char_contributions.entry(char_id).or_insert(0) += acc;
            if det.auto_payout_on_contribution {
                *det.contributions_by_char.entry(char_id).or_insert(0) += acc;
            }
            let u = <[u8; 16]>::try_from(det.uuid_bytes.as_slice())
                .unwrap_or_else(|_| if is_fl { freelance_uuid(det.created_unix, pid) } else { corp_goal_uuid(pid) });
            (
                acc,
                det.auto_payout_on_contribution,
                det.wallet_division.clamp(1, 7),
                det.creator_id,
                u,
                det.char_contributions.len(),
            )
        };
        let reward = if auto_payout {
            proj.isk_per_unit
                .checked_mul_qty(accepted as i64)
                .unwrap_or(IskCents::ZERO)
                .min(proj.escrow_remaining)
        } else {
            IskCents::ZERO
        };
        let prev_cur = proj.current_quantity;
        let mut completed = false;
        let mut new_cur = prev_cur;
        if let Some(proj_mut) = snap.corp_projects.get_mut(&pid) {
            proj_mut.current_quantity += accepted;
            new_cur = proj_mut.current_quantity;
            if reward.raw_cents() > 0 {
                proj_mut.escrow_remaining = proj_mut
                    .escrow_remaining
                    .checked_sub(reward)
                    .unwrap_or(IskCents::ZERO);
            }
            completed = proj_mut.current_quantity >= proj_mut.target_quantity;
        }
        if completed {
            if let Some(m) = snap.alliance_war_state.project_meta.get_mut(&pid) {
                m.2 = true;
            }
            if let Some(d) = snap.alliance_war_state.project_details.get_mut(&pid) {
                d.status = "Completed".into();
            }
        }
        sync_corp_project_contributors(snap, pid);
        let mut auto_paid_isk = 0u64;
        if reward.raw_cents() > 0 {
            auto_paid_isk = (reward.raw_cents() / 100).max(1) as u64;
            let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = ch.balance.checked_add(reward).unwrap_or(ch.balance);
                ch.balance
            } else {
                reward
            };
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: char_id,
                entry_type: "corporation_project_reward".into(),
                amount: reward,
                balance_after: bal_after,
                reason: format!("Project {pid} ({}) contribution reward", proj.name),
            });
            snap.wallet_journal_timestamps.insert(jid, now_unix);

            let corp_bal = snap
                .corporations
                .get(&proj.corporation_id)
                .map(|c| if div_id == 1 { c.master_wallet } else { c.division_balance(div_id) })
                .unwrap_or(IskCents::ZERO);
            append_corp_journal(
                snap,
                proj.corporation_id,
                div_id,
                now_unix,
                "CorporationProjectPayout",
                format!("Project {pid} ({}) payout to pilot {char_id}", proj.name),
                IskCents(-reward.raw_cents()),
                corp_bal,
                Some(char_id),
                "Corporation project contribution reward",
            );
        }
        updates.push(ProjectActivityUpdate {
            pid,
            corp_id: proj.corporation_id,
            creator_id,
            is_freelance: is_fl,
            uuid,
            prev_cur,
            new_cur,
            completed,
            total_contributors,
            auto_paid_isk,
        });
    }
    (expired, updates)
}

pub fn record_server_project_activity_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    matcher_types: &[&str],
    qty: u64,
    filter_opt: Option<i64>,
    now_unix: u64,
) -> Vec<(i64, u64, bool)> {
    let (_expired, updates) = record_server_project_activity_detailed(
        snap,
        char_id,
        matcher_types,
        qty,
        filter_opt,
        now_unix,
    );
    updates
        .into_iter()
        .map(|u| (u.pid, u.new_cur, u.completed))
        .collect()
}

pub fn record_server_project_activity(
    ctx: &ServerContext,
    char_id: i64,
    matcher_types: &[&str],
    qty: u64,
    filter_opt: Option<i64>,
) {
    let now_unix = ctx.now_unix();
    let (expired, updates) = ctx.db.with_mut_snapshot(|snap| {
        record_server_project_activity_detailed(
            snap,
            char_id,
            matcher_types,
            qty,
            filter_opt,
            now_unix,
        )
    });
    emit_expired_quasar_notices(ctx, &expired, now_unix);
    if updates.is_empty() {
        return;
    }

    {
        let mut ggoals = ctx
            .quasar_gateway
            .progression
            .extra
            .generic_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for u in &updates {
            if !u.is_freelance {
                let hex = crate::services::quasar_progression_extra::uuid_to_hex(&u.uuid);
                if let Some(g) = ggoals.get_mut(&hex) {
                    g.current_progress = u.new_cur.min(g.desired_progress);
                    changed = true;
                }
            }
        }
        drop(ggoals);
        if changed {
            ctx.quasar_gateway.progression.extra.save_to_db();
        }
    }

    let contributor_u32 = u32::try_from(char_id).unwrap_or(0);
    for u in updates {
        if u.is_freelance {
            if u.corp_id > 0 {
                let mut id_b = Vec::with_capacity(18);
                encode_bytes_field(1, &u.uuid, &mut id_b);
                if contributor_u32 > 0 {
                    let char_tot = ctx.db.with_snapshot(|s| {
                        s.alliance_war_state
                            .project_details
                            .get(&u.pid)
                            .and_then(|d| d.char_contributions.get(&char_id).copied())
                            .unwrap_or(u.new_cur)
                    });
                    let mut cid_b = Vec::with_capacity(8);
                    encode_varint_field(1, u64::from(contributor_u32), &mut cid_b);
                    let mut contrib_b = Vec::new();
                    encode_bytes_field(1, &id_b, &mut contrib_b);
                    encode_bytes_field(2, &cid_b, &mut contrib_b);
                    encode_varint_field(3, char_tot, &mut contrib_b);
                    let _ = ctx.quasar_gateway.publish_notice(
                        NoticeScope::Corporation(u.corp_id as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantContributedNotice"),
                            value: contrib_b,
                        },
                        now_unix,
                    );
                }
                let mut prog_b = Vec::new();
                encode_bytes_field(1, &id_b, &mut prog_b);
                encode_varint_field(2, u.new_cur, &mut prog_b);
                encode_varint_field(3, u.total_contributors.max(1) as u64, &mut prog_b);
                let _ = ctx.quasar_gateway.publish_notice(
                    NoticeScope::Corporation(u.corp_id as u32),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ProgressedNotice"),
                        value: prog_b,
                    },
                    now_unix,
                );
                if u.completed && u.prev_cur < u.new_cur {
                    let mut comp_b = Vec::with_capacity(20);
                    encode_bytes_field(1, &id_b, &mut comp_b);
                    let _ = ctx.quasar_gateway.publish_notice(
                        NoticeScope::Corporation(u.corp_id as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CompletedNotice"),
                            value: comp_b,
                        },
                        now_unix,
                    );
                }
            }
        } else {
            let scope = notices::goal_scope(
                (u.corp_id > 0).then_some(u.corp_id as u32),
                u.creator_id.max(0) as u32,
            );
            let _ = ctx.quasar_gateway.publish_notice(
                scope.clone(),
                notices::goal_contributed(&u.uuid, contributor_u32, u.prev_cur, u.new_cur, now_unix),
                now_unix,
            );
            if u.completed && u.prev_cur < u.new_cur {
                let _ = ctx
                    .quasar_gateway
                    .publish_notice(scope, notices::goal_completed(&u.uuid), now_unix);
            }
            if u.auto_paid_isk > 0 && contributor_u32 > 0 {
                let mut id_b = Vec::with_capacity(18);
                encode_bytes_field(1, &u.uuid, &mut id_b);
                let mut red_b = Vec::with_capacity(28);
                encode_bytes_field(1, &id_b, &mut red_b);
                encode_varint_field(2, u.auto_paid_isk, &mut red_b);
                let _ = ctx.quasar_gateway.publish_notice(
                    NoticeScope::Character(contributor_u32),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.RedeemedNotice"),
                        value: red_b,
                    },
                    now_unix,
                );
            }
        }
    }
}

pub fn handle_edit_project(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session.corporation_id.unwrap_or(0);
    let pid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let new_name_opt = args.get(1).and_then(|a| a.as_str()).map(String::from);
    let new_expires_opt = args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 0).map(|v| v as u64);
    let new_cap_opt = args.get(3).and_then(|a| a.as_i64()).filter(|&v| v > 0).map(|v| v as u64);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(proj) = snap.corp_projects.get(&pid) else {
            return Err(BridgeRpcError {
                code: "ProjectNotFound".into(),
                message: format!("Corporation project {pid} not found"),
            });
        };
        if proj.corporation_id != corp_id {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot edit a project belonging to another corporation".into(),
            });
        }
        let is_gm = session.has_gm_role();
        let (is_ceo, caller_roles) =
            effective_corp_member_roles(snap, corp_id, char_id, session.corp_role, ctx.now_unix());
        if snap.corporations.contains_key(&corp_id)
            && !is_ceo
            && !is_gm
            && !corp_roles::has_role(caller_roles, corp_roles::ROLE_DIRECTOR)
            && !corp_roles::has_role(caller_roles, corp_roles::ROLE_PROJECT_MANAGER)
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Editing corporation projects requires Project Manager or Director role".into(),
            });
        }
        if let Some(new_name) = &new_name_opt {
            if let Some(p) = snap.corp_projects.get_mut(&pid) {
                p.name = new_name.clone();
            }
        }
        let det = snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_default();
        if let Some(exp) = new_expires_opt {
            det.expires_unix = Some(exp);
        }
        if let Some(cap) = new_cap_opt {
            det.max_per_character = Some(cap);
        }
        Ok((
            snap.corp_projects.get(&pid).map(|p| p.name.clone()).unwrap_or_default(),
            det.creator_id,
        ))
    });
    match res {
        Ok((updated_name, creator_id)) => {
            if new_name_opt.is_some() {
                let uuid = corp_goal_uuid(pid);
                let _ = ctx.quasar_gateway.publish_notice(
                    notices::goal_scope((corp_id > 0).then_some(corp_id as u32), creator_id.max(0) as u32),
                    notices::goal_name_changed(&uuid, &updated_name),
                    ctx.now_unix(),
                );
            }
            Some(Ok(PyRep::keyval([
                ("projectID", PyRep::Integer(pid)),
                ("name", PyRep::String(updated_name)),
                ("updated", PyRep::Bool(true)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}
