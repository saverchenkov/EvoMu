//! `fw_enlistment` — Faction Warfare / Insurgency militia enlistment (`fwCharacterEnlistmentMgr`).
//!
//! Implements the V24 client surface used by `eve/client/script/ui/services/fwEnlistmentSvc.pyj`:
//! `GetMyEnlistment`, `CreateMyDirectEnlistment`, `RemoveMyDirectEnlistment`,
//! `GetMyDirectEnlistmentCooldownTimestamp`, `GetCorpAllowedEnlistmentFactions` and
//! `SetMyCorpAllowedEnlistmentFactions`.
//!
//! Militia factions are derived from the SDE (`sde_factions.militia_corporation_id IS NOT NULL`);
//! there is no hardcoded faction table here.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Private JSON side-store key in `item_custom_names` (same pattern as the account/char stores).
pub(crate) const FW_ENLISTMENT_STORE_KEY: i64 = -10_000_003;

/// DERIVED: cooldown applied after leaving a militia via `RemoveMyDirectEnlistment`.
/// The exact live-server value is not present in the client or SDE; 24 hours is an assumption
/// documented here so it can be replaced once a primary source is available.
pub(crate) const DIRECT_ENLISTMENT_COOLDOWN_SECS: u64 = 24 * 3_600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FwEnlistmentStore {
    /// characterID -> unix time until which direct enlistment is blocked.
    #[serde(default)]
    pub cooldown_until: HashMap<i64, u64>,
    /// corporationID -> allowed militia factionIDs set by a director (player corps only).
    #[serde(default)]
    pub corp_allowed: HashMap<i64, Vec<i64>>,
    /// corporationID -> unix time until which corporation/alliance FW re-enlistment is blocked after leaving.
    #[serde(default)]
    pub corp_cooldown_until: HashMap<i64, u64>,
    /// characterID -> forced `war_faction_id` update to apply to live `SessionState` on next RPC.
    #[serde(default)]
    pub forced_war_faction_sync: HashMap<i64, Option<i64>>,
}

/// Tuple `(char_id, old_wf, new_eff, new_direct, active_ship_id, ship_type_id)` returned on corp FW mutations.
pub(crate) type FwMemberMutation = (i64, Option<i64>, Option<i64>, Option<i64>, i64, u32);

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Unix seconds -> Windows FILETIME ticks (100ns since 1601), the blue time format the client reads.
pub(crate) fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

pub(crate) fn load_store(snap: &eve_db::WorldStateSnapshot) -> FwEnlistmentStore {
    snap.item_custom_names
        .get(&FW_ENLISTMENT_STORE_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_store(snap: &mut eve_db::WorldStateSnapshot, store: &FwEnlistmentStore) {
    if let Ok(json) = serde_json::to_string(store) {
        snap.item_custom_names.insert(FW_ENLISTMENT_STORE_KEY, json);
    }
}

pub(crate) fn take_forced_war_faction_sync(ctx: &ServerContext, char_id: i64) -> Option<Option<i64>> {
    if !ctx.db.with_snapshot(|s| load_store(s).forced_war_faction_sync.contains_key(&char_id)) {
        return None;
    }
    ctx.db.with_mut_snapshot(|s| {
        let mut st = load_store(s);
        let res = st.forced_war_faction_sync.remove(&char_id);
        if res.is_some() {
            save_store(s, &st);
        }
        res
    })
}

pub(crate) fn is_corp_leave_cooldown_active_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    now_unix: u64,
) -> bool {
    load_store(snap)
        .corp_cooldown_until
        .get(&corp_id)
        .copied()
        .is_some_and(|until| now_unix < until)
}

pub(crate) fn record_corp_leave_cooldown_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    until_unix: u64,
) {
    let mut st = load_store(snap);
    st.corp_cooldown_until.insert(corp_id, until_unix);
    save_store(snap, &st);
}

pub(crate) fn apply_corp_militia_faction_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    fac: Option<i64>,
    now_unix: u64,
) -> Vec<FwMemberMutation> {
    if let Some(f) = fac {
        snap.fw_runtime.corp_war_factions.insert(cid, f);
    } else {
        snap.fw_runtime.corp_war_factions.remove(&cid);
    }
    let member_ids: Vec<i64> = snap
        .characters
        .values()
        .filter(|ch| ch.corporation_id == cid)
        .map(|ch| ch.character_id)
        .collect();
    let mut store = load_store(snap);
    if let Some(f) = fac {
        if let Some(allowed) = store.corp_allowed.get_mut(&cid) {
            allowed.retain(|&af| af == f);
        }
    }
    let mut mutations = Vec::with_capacity(member_ids.len());
    for mid in member_ids {
        let mut direct_opt = snap.fw_runtime.direct_enlisted_chars.get(&mid).copied();
        let eff = if let Some(f) = fac {
            if direct_opt.is_some_and(|df| df != f) {
                snap.fw_runtime.direct_enlisted_chars.remove(&mid);
                direct_opt = None;
            }
            Some(f)
        } else {
            direct_opt
        };
        let (old_wf, ship_id) = if let Some(ch) = snap.characters.get_mut(&mid) {
            let old = ch.war_faction_id;
            ch.war_faction_id = eff;
            ch.version += 1;
            (old, ch.active_ship_id)
        } else {
            continue;
        };
        let ship_tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        if let Some(f) = eff {
            let rec = snap.fw_character_stats.entry(mid).or_insert_with(|| {
                eve_db::snapshot_ext::FwCharacterStatsRecord {
                    character_id: mid,
                    faction_id: Some(f),
                    enlisted_on_unix: Some(now_unix),
                    current_rank: 1,
                    highest_rank: 1,
                    kills: Default::default(),
                    victory_points: Default::default(),
                }
            });
            rec.faction_id = Some(f);
            if rec.enlisted_on_unix.is_none() {
                rec.enlisted_on_unix = Some(now_unix);
            }
            rec.current_rank = rec.current_rank.max(1);
            rec.highest_rank = rec.highest_rank.max(rec.current_rank);
        } else if let Some(rec) = snap.fw_character_stats.get_mut(&mid) {
            rec.faction_id = None;
            rec.enlisted_on_unix = None;
            rec.current_rank = 0;
        }
        store.forced_war_faction_sync.insert(mid, eff);
        mutations.push((mid, old_wf, eff, direct_opt, ship_id, ship_tid));
    }
    save_store(snap, &store);
    mutations
}

pub(crate) fn notify_member_war_faction_change(
    ctx: &ServerContext,
    char_id: i64,
    old_fac: Option<i64>,
    effective: Option<i64>,
    direct: Option<i64>,
    corp: Option<i64>,
) {
    notify_state(ctx, char_id, effective, direct, corp);
    if old_fac != effective {
        let old_py = old_fac.map(PyRep::Integer).unwrap_or(PyRep::None);
        let new_py = effective.map(PyRep::Integer).unwrap_or(PyRep::None);
        let sess_rep = PyRep::keyval([
            ("charid", PyRep::Integer(char_id)),
            ("warfactionid", new_py.clone()),
        ]);
        let change_args = vec![
            PyRep::Bool(false),
            sess_rep,
            PyRep::Dict(vec![(
                PyRep::String("warfactionid".into()),
                PyRep::Tuple(vec![old_py, new_py]),
            )]),
        ];
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSessionChanged",
            "charid",
            change_args,
        );
    }
}

pub(crate) async fn sync_corp_militia_members_after_change(
    ctx: &ServerContext,
    session: &mut SessionState,
    corp_fac: Option<i64>,
    changes: &[FwMemberMutation],
) {
    for &(mid, old_wf, eff, direct_opt, ship_id, ship_tid) in changes {
        if session.character_id == Some(mid) {
            session.war_faction_id = eff;
            let _ = take_forced_war_faction_sync(ctx, mid);
        }
        notify_member_war_faction_change(ctx, mid, old_wf, eff, direct_opt, corp_fac);
        if old_wf != eff && ship_id > 0 && ship_tid > 0 {
            if let Some(sys_id) = ctx.systems.find_ball_system_sync(ship_id) {
                let meta = ctx.build_ship_slim_meta(mid, ship_tid);
                let _ = ctx
                    .systems
                    .get_or_spawn(sys_id)
                    .change_slim_item(ship_id, meta)
                    .await;
            }
        }
    }
}

fn require_docked_for_direct_enlistment(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
) -> Result<(), BridgeRpcError> {
    let station_id = session
        .station_id
        .or_else(|| if session.ship_id.is_none() { ch.station_id } else { None });
    let structure_id = session
        .structure_id
        .or_else(|| if session.ship_id.is_none() { ch.structure_id } else { None });
    if station_id.is_some() {
        return Ok(());
    }
    if let Some(sid) = structure_id {
        let st = snap.structures.get(&sid).ok_or_else(|| {
            err(
                "MustBeDocked",
                "Docked structure not found for direct militia enlistment",
            )
        })?;
        if matches!(
            st.state.as_str(),
            "Destroyed" | "Unanchored" | "Anchoring" | "AnchorVulnerable"
        ) || !st.quantum_core_installed
        {
            return Err(err(
                "MustBeDocked",
                "Structure must be online and cored to change direct militia enlistment",
            ));
        }
        return Ok(());
    }
    Err(err(
        "MustBeDocked",
        "Must be docked in a station or operational structure to change direct militia enlistment",
    ))
}

/// Militia factions derived from the SDE: every faction that has a militia corporation.
pub(crate) fn is_militia_faction(ctx: &ServerContext, faction_id: i64) -> bool {
    u32::try_from(faction_id)
        .ok()
        .and_then(|fid| ctx.sde.factions.get(&fid))
        .is_some_and(|f| f.militia_corporation_id.is_some())
}

fn enlistment_tuple_full(
    effective: Option<i64>,
    direct: Option<i64>,
    corp: Option<i64>,
) -> PyRep {
    let e = effective.map(PyRep::Integer).unwrap_or(PyRep::None);
    let d = direct.map(PyRep::Integer).unwrap_or(PyRep::None);
    let c = corp.map(PyRep::Integer).unwrap_or(PyRep::None);
    PyRep::Tuple(vec![e, d, c])
}

fn notify_state(
    ctx: &ServerContext,
    char_id: i64,
    effective: Option<i64>,
    direct: Option<i64>,
    corp: Option<i64>,
) {
    let e = effective.map(PyRep::Integer).unwrap_or(PyRep::None);
    let d = direct.map(PyRep::Integer).unwrap_or(PyRep::None);
    let c = corp.map(PyRep::Integer).unwrap_or(PyRep::None);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEnlistmentStateUpdated",
        "charid",
        vec![e, d, c],
    );
}

fn faction_list_arg(ctx: &ServerContext, arg: Option<&PyRep>) -> Result<Vec<i64>, BridgeRpcError> {
    let items: Vec<PyRep> = match arg {
        Some(PyRep::List(v)) | Some(PyRep::Tuple(v)) => v.clone(),
        Some(PyRep::None) | None => Vec::new(),
        Some(other) => return Err(err("InvalidFactionList", format!("Expected list of factionIDs, got {other:?}"))),
    };
    let mut out = Vec::new();
    for it in items {
        let fid = it.as_i64().ok_or_else(|| err("InvalidFactionList", "factionIDs must be integers"))?;
        if !is_militia_faction(ctx, fid) {
            return Err(err("InvalidWarFaction", format!("Faction {fid} has no militia")));
        }
        if !out.contains(&fid) {
            out.push(fid);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Returns `Some(result)` when `(service, method)` belongs to `fwCharacterEnlistmentMgr`.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "fwCharacterEnlistmentMgr" {
        return None;
    }
    let res = match method {
        "GetMyEnlistment" => get_my_enlistment(ctx, session),
        "CreateMyDirectEnlistment" => create_direct(ctx, session, args),
        "RemoveMyDirectEnlistment" => remove_direct(ctx, session),
        "GetMyDirectEnlistmentCooldownTimestamp" => cooldown_timestamp(ctx, session),
        "GetCorpAllowedEnlistmentFactions" => get_corp_allowed(ctx, session, args),
        "SetMyCorpAllowedEnlistmentFactions" => set_corp_allowed(ctx, session, args),
        _ => return None,
    };
    Some(res)
}

fn get_my_enlistment(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (eff, direct, corp) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id);
        let corp_fac = ch.and_then(|c| s.fw_runtime.corp_war_factions.get(&c.corporation_id).copied());
        let direct_fac = s.fw_runtime.direct_enlisted_chars.get(&char_id).copied();
        let eff_fac = ch.and_then(|c| c.war_faction_id).or(direct_fac).or(corp_fac);
        let resolved_direct = if direct_fac.is_none() && corp_fac.is_none() {
            eff_fac
        } else {
            direct_fac
        };
        (eff_fac, resolved_direct, corp_fac)
    });
    Ok(enlistment_tuple_full(eff, direct, corp))
}

fn create_direct(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let faction_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| err("InvalidWarFaction", "CreateMyDirectEnlistment requires a factionID"))?;
    if !is_militia_faction(ctx, faction_id) {
        return Err(err("InvalidWarFaction", format!("Faction {faction_id} has no militia")));
    }
    let now = ctx.now_unix();
    let militia_corp_opt = u32::try_from(faction_id)
        .ok()
        .and_then(|fid| ctx.sde.factions.get(&fid))
        .and_then(|f| f.militia_corporation_id)
        .map(i64::from);
    let moved_corp_opt = ctx.db.try_with_mut_snapshot(|snap| {
        let store = load_store(snap);
        let ch = snap
            .characters
            .get(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        require_docked_for_direct_enlistment(snap, session, ch)?;
        if let Some(current) = ch.war_faction_id {
            return Err(err("FwAlreadyEnlisted", format!("Character is already enlisted with faction {current}")));
        }
        if let Some(&until) = store.cooldown_until.get(&char_id) {
            if now < until {
                return Err(err("FwEnlistmentCooldown", format!("Direct enlistment is on cooldown until {until}")));
            }
        }
        let standing = ch.standings.get(&faction_id).copied().unwrap_or(0.0);
        if standing < 0.0 {
            return Err(err(
                "FwStandingTooLow",
                format!("Standing {standing:.2} towards faction {faction_id} must be non-negative"),
            ));
        }
        let old_corp_id = ch.corporation_id;
        let in_player_corp = snap.corporations.contains_key(&old_corp_id);
        // Player corporations may restrict which militias their members can join, and cannot mix opposing militias.
        if in_player_corp {
            if let Some(&corp_fac) = snap.fw_runtime.corp_war_factions.get(&old_corp_id) {
                if corp_fac != faction_id {
                    return Err(err(
                        "FwFactionNotAllowedByCorp",
                        format!("Corporation {old_corp_id} is enlisted with faction {corp_fac} and cannot allow opposing direct enlistment"),
                    ));
                }
            }
            if let Some(allowed) = store.corp_allowed.get(&old_corp_id) {
                if !allowed.contains(&faction_id) {
                    return Err(err(
                        "FwFactionNotAllowedByCorp",
                        format!("Corporation {} does not allow enlistment with faction {faction_id}", old_corp_id),
                    ));
                }
            }
        }
        let mut new_corp_opt = None;
        let ch = snap.characters.get_mut(&char_id).expect("checked above");
        ch.war_faction_id = Some(faction_id);
        if !in_player_corp {
            if let Some(militia_corp_id) = militia_corp_opt {
                if militia_corp_id != old_corp_id {
                    ch.corporation_id = militia_corp_id;
                    new_corp_opt = Some((old_corp_id, militia_corp_id));
                }
            }
        }
        ch.version += 1;
        let released = if let Some((old_cid, new_cid)) = new_corp_opt {
            crate::services::fleet_social::fleet_social_corps::record_corp_join_in_snap_pub(
                snap,
                char_id,
                old_cid,
                new_cid,
                now,
            )
        } else {
            Vec::new()
        };
        let rec = snap.fw_character_stats.entry(char_id).or_insert_with(|| {
            eve_db::snapshot_ext::FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(faction_id),
                enlisted_on_unix: Some(now),
                current_rank: 1,
                highest_rank: 1,
                kills: Default::default(),
                victory_points: Default::default(),
            }
        });
        rec.faction_id = Some(faction_id);
        rec.enlisted_on_unix = Some(now);
        snap.fw_runtime.direct_enlisted_chars.insert(char_id, faction_id);
        Ok((new_corp_opt, released))
    })?;
    let (moved_corp_opt, released) = (moved_corp_opt.0, moved_corp_opt.1);
    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    if let Some((_, new_cid)) = moved_corp_opt {
        session.corporation_id = Some(new_cid);
    }
    session.war_faction_id = Some(faction_id);
    let corp_fac = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| s.fw_runtime.corp_war_factions.get(&c.corporation_id).copied())
    });
    notify_member_war_faction_change(ctx, char_id, None, Some(faction_id), Some(faction_id), corp_fac);
    Ok(PyRep::None)
}

fn remove_direct(ctx: &ServerContext, session: &mut SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let until = now + DIRECT_ENLISTMENT_COOLDOWN_SECS;
    let (moved_corp_opt, old_wf, inherited_corp_fac, released) = ctx.db.try_with_mut_snapshot(|snap| {
        let ch_ref = snap
            .characters
            .get(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        require_docked_for_direct_enlistment(snap, session, ch_ref)?;
        if !snap.fw_runtime.direct_enlisted_chars.contains_key(&char_id) {
            return Err(err("FwNotEnlisted", "Character is not directly enlisted"));
        }
        let old_corp_id = ch_ref.corporation_id;
        let in_player_corp = snap.corporations.contains_key(&old_corp_id);
        let default_npc_corp = if !in_player_corp {
            Some(crate::services::fleet_social::fleet_social_corp_tail::bloodline_default_npc_corp(
                ctx, snap, char_id,
            ))
        } else {
            None
        };
        let corp_fac = snap.fw_runtime.corp_war_factions.get(&old_corp_id).copied();
        let ch = snap.characters.get_mut(&char_id).expect("checked above");
        let old_wf = ch.war_faction_id;
        ch.war_faction_id = corp_fac;
        let mut new_corp_opt = None;
        if let Some(def_corp) = default_npc_corp {
            if def_corp != old_corp_id {
                ch.corporation_id = def_corp;
                new_corp_opt = Some((old_corp_id, def_corp));
            }
        }
        ch.version += 1;
        snap.fw_runtime.direct_enlisted_chars.remove(&char_id);
        if let Some(st) = snap.fw_character_stats.get_mut(&char_id) {
            st.faction_id = corp_fac;
            if corp_fac.is_none() {
                st.enlisted_on_unix = None;
                st.current_rank = 0;
            }
        }
        let released = if let Some((old_cid, new_cid)) = new_corp_opt {
            crate::services::fleet_social::fleet_social_corps::record_corp_join_in_snap_pub(
                snap,
                char_id,
                old_cid,
                new_cid,
                now,
            )
        } else {
            Vec::new()
        };
        let mut store = load_store(snap);
        store.cooldown_until.insert(char_id, until);
        save_store(snap, &store);
        Ok((new_corp_opt, old_wf, corp_fac, released))
    })?;
    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    if let Some((_, new_cid)) = moved_corp_opt {
        session.corporation_id = Some(new_cid);
    }
    session.war_faction_id = inherited_corp_fac;
    notify_member_war_faction_change(ctx, char_id, old_wf, inherited_corp_fac, None, inherited_corp_fac);
    Ok(PyRep::None)
}

fn cooldown_timestamp(ctx: &ServerContext, session: &SessionState) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let mut expired = false;
    let until = ctx.db.with_mut_snapshot(|s| {
        let mut store = load_store(s);
        let u = store.cooldown_until.get(&char_id).copied();
        if let Some(ts) = u {
            if ts <= now {
                store.cooldown_until.remove(&char_id);
                save_store(s, &store);
                expired = true;
            }
        }
        u
    });
    if expired {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnEnlistmentCooldownCleared",
            "charid",
            vec![],
        );
    }
    Ok(match until {
        Some(u) if u > now => PyRep::Integer(unix_to_filetime(u)),
        _ => PyRep::None,
    })
}

fn get_corp_allowed(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let corp_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.corporation_id)
        .ok_or_else(|| err("CorporationNotFound", "GetCorpAllowedEnlistmentFactions requires a corporationID"))?;
    let allowed = ctx.db.with_snapshot(|s| load_store(s).corp_allowed.get(&corp_id).cloned());
    Ok(match allowed {
        Some(list) => PyRep::List(list.into_iter().map(PyRep::Integer).collect()),
        None => PyRep::List(Vec::new()),
    })
}

fn set_corp_allowed(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let factions = faction_list_arg(ctx, args.first())?;
    let now = ctx.now_unix();
    let pre_cid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
        .unwrap_or(0);
    if pre_cid > 0 {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, pre_cid);
    }
    let (corp_id, corp_fac, purged_members) = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        if !snap.corporations.contains_key(&corp_id) {
            return Err(err("CorpRoleDenied", "NPC corporations cannot configure militia enlistment"));
        }
        let (is_ceo, role) =
            crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                snap, corp_id, char_id, 0, now,
            );
        if !is_ceo && !corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR) {
            return Err(err("CorpRoleDenied", "Only the CEO or a director may set allowed enlistment factions"));
        }
        let corp_fac = snap.fw_runtime.corp_war_factions.get(&corp_id).copied();
        if let Some(cf) = corp_fac {
            if factions.iter().any(|&f| f != cf) {
                return Err(err(
                    "FwFactionNotAllowedByCorp",
                    format!("Corporation {corp_id} is enlisted with faction {cf} and cannot allow opposing militias"),
                ));
            }
        }
        let mut store = load_store(snap);
        store.corp_allowed.insert(corp_id, factions.clone());
        let disallowed_members: Vec<i64> = snap
            .characters
            .values()
            .filter(|ch| ch.corporation_id == corp_id)
            .filter_map(|ch| {
                snap.fw_runtime
                    .direct_enlisted_chars
                    .get(&ch.character_id)
                    .copied()
                    .filter(|df| !factions.contains(df))
                    .map(|_| ch.character_id)
            })
            .collect();
        let mut purged = Vec::with_capacity(disallowed_members.len());
        for mid in disallowed_members {
            snap.fw_runtime.direct_enlisted_chars.remove(&mid);
            let (old_wf, ship_id) = if let Some(ch) = snap.characters.get_mut(&mid) {
                let old = ch.war_faction_id;
                ch.war_faction_id = corp_fac;
                ch.version += 1;
                (old, ch.active_ship_id)
            } else {
                continue;
            };
            let ship_tid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
            if let Some(st) = snap.fw_character_stats.get_mut(&mid) {
                st.faction_id = corp_fac;
                if corp_fac.is_none() {
                    st.enlisted_on_unix = None;
                    st.current_rank = 0;
                }
            }
            store.forced_war_faction_sync.insert(mid, corp_fac);
            purged.push((mid, old_wf, corp_fac, None::<i64>, ship_id, ship_tid));
        }
        save_store(snap, &store);
        Ok((corp_id, corp_fac, purged))
    })?;
    for &(mid, old_wf, eff, direct_opt, ship_id, ship_tid) in &purged_members {
        if session.character_id == Some(mid) {
            session.war_faction_id = eff;
            let _ = take_forced_war_faction_sync(ctx, mid);
        }
        notify_member_war_faction_change(ctx, mid, old_wf, eff, direct_opt, corp_fac);
        if old_wf != eff && ship_id > 0 && ship_tid > 0 {
            if let Some(sys_id) = ctx.systems.find_ball_system_sync(ship_id) {
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    let sys_h = ctx.systems.get_or_spawn(sys_id);
                    let meta = ctx.build_ship_slim_meta(mid, ship_tid);
                    rt.spawn(async move {
                        let _ = sys_h.change_slim_item(ship_id, meta).await;
                    });
                }
            }
        }
    }
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnCorpAllowedEnlistmentFactionsSet",
        "corpid",
        vec![PyRep::List(factions.iter().copied().map(PyRep::Integer).collect())],
    );
    Ok(PyRep::List(factions.into_iter().map(PyRep::Integer).collect()))
}

#[cfg(test)] #[path = "fw_enlistment_tests.rs"] mod fw_enlistment_tests;
#[cfg(test)] #[path = "fw_enlistment_edges_tests.rs"] mod fw_enlistment_edges_tests;

