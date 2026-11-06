//! Live-DB corporation membership & role authorization helpers for `quasar_corp_domain`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{require_admin_role, STATUS_FORBIDDEN};
use crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles;
use eve_db::{corp_roles, WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

pub(super) fn live_caller_corp_in_snap(
    ctx: &AuthoritativeContext,
    snap: &WorldStateSnapshot,
    cid: i64,
) -> Option<i64> {
    let ctx_corp = ctx.effective_corporation_id().map(i64::from);
    match snap.characters.get(&cid) {
        Some(ch) if (1_000_000..2_000_000).contains(&ch.corporation_id) => {
            if let Some(cc) = ctx_corp.filter(|&cc| cc != ch.corporation_id && cc > 0) {
                if snap
                    .corporations
                    .get(&cc)
                    .is_some_and(|c| c.ceo_id == cid || c.members.contains(&cid))
                {
                    return Some(cc);
                }
            }
            Some(ch.corporation_id)
        }
        Some(ch) if ch.corporation_id > 0 => Some(ch.corporation_id),
        _ => ctx_corp,
    }
}

pub(crate) fn live_caller_corp(ctx: &AuthoritativeContext, db: &WorldRepository) -> Option<i64> {
    let cid = ctx.effective_character_id()? as i64;
    db.with_snapshot(|s| live_caller_corp_in_snap(ctx, s, cid))
}

pub(crate) fn resolve_authorized_corp(
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
    corp_field: u32,
    db: &WorldRepository,
) -> Result<i64, (u32, String)> {
    let is_admin = require_admin_role(ctx).is_ok();
    let caller_cid = ctx.effective_character_id();
    if caller_cid.is_none() && !is_admin {
        return Err((STATUS_FORBIDDEN, "Character authentication required".into()));
    }
    let caller_corp = live_caller_corp(ctx, db)
        .or_else(|| is_admin.then(|| ctx.effective_corporation_id().map(i64::from)).flatten())
        .and_then(|c| u32::try_from(c).ok());
    let req_corp = super::extract_id(msg, corp_field)
        .or_else(|| if corp_field != 1 { super::extract_id(msg, 1) } else { None });
    let target = match (req_corp, caller_corp) {
        (Some(req), Some(own)) => {
            if req != own && !is_admin {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }
            req
        }
        (Some(req), None) if is_admin => req,
        (None, Some(own)) => own,
        _ => return Err((STATUS_FORBIDDEN, "Corporation context required".into())),
    };
    Ok(target as i64)
}

pub(crate) fn check_corp_admin_role(
    ctx: &AuthoritativeContext,
    db: &WorldRepository,
) -> Result<(u32, i64), (u32, String)> {
    let Some(cid) = ctx.effective_character_id() else {
        return Err((STATUS_FORBIDDEN, "Authentication required".into()));
    };
    let corpid = live_caller_corp(ctx, db)
        .ok_or_else(|| (STATUS_FORBIDDEN, "Missing corporation context".into()))?;
    let now_u = ctx.now_unix();
    let is_admin = require_admin_role(ctx).is_ok()
        || db.with_snapshot(|s| {
            let (is_ceo, eff) = effective_corp_member_roles(s, corpid, cid as i64, 0, now_u);
            is_ceo
                || (eff & (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER)) != 0
        });
    if !is_admin {
        return Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()));
    }
    Ok((cid, corpid))
}

pub(crate) fn require_corp_roles(
    ctx: &AuthoritativeContext,
    db: &WorldRepository,
    corpid: i64,
    allowed_roles: u64,
) -> Result<(), (u32, String)> {
    if require_admin_role(ctx).is_ok() {
        return Ok(());
    }
    let Some(cid) = ctx.effective_character_id() else {
        return Err((STATUS_FORBIDDEN, "Authentication required".into()));
    };
    let now_u = ctx.now_unix();
    let authorized = db.with_snapshot(|s| {
        if live_caller_corp_in_snap(ctx, s, cid as i64) != Some(corpid) {
            return false;
        }
        let (is_ceo, eff) = effective_corp_member_roles(s, corpid, cid as i64, 0, now_u);
        is_ceo || (eff & (corp_roles::ROLE_DIRECTOR | allowed_roles)) != 0
    });
    if !authorized {
        return Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()));
    }
    Ok(())
}
