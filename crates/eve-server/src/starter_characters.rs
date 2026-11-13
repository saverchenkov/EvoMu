//! Startup provisioning of starter characters for configured player accounts (T-6 harness).
//!
//! A `[[player_accounts]]` entry may carry a `starter_character`. When that account owns no
//! character yet, the server creates one through the same `charUnboundMgr.CreateCharacterWithDoll`
//! handler the real client uses, so the character gets its school station, starter ship, skills and
//! ISK from the normal creation path (no hand-built records).

#![forbid(unsafe_code)]

use crate::config::ServerConfig;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use tracing::{info, warn};

tokio::task_local! {
    static SKIP_DEFAULT_STARTER_PLEX: ();
}

/// Returns `true` when `CreateCharacterWithDoll` should apply `feature_flags.starter_plex_balance`
/// directly (disabled while `provision_starter_characters` runs so `initial_balance_plex` wins).
pub(crate) fn should_grant_default_starter_plex() -> bool {
    SKIP_DEFAULT_STARTER_PLEX.try_with(|_| ()).is_err()
}

/// Credits `plex_grant` PLEX (if positive, once per `user_id`) to `(user_id, char_id)` inside an
/// active world-state snapshot mutation and records a `"Starter PLEX Grant"` ledger entry.
pub(crate) fn grant_starter_plex_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    user_id: u64,
    char_id: i64,
    plex_grant: i64,
    now_unix: u64,
) {
    if plex_grant <= 0 {
        return;
    }
    let uid_i64 = user_id as i64;
    if snap
        .kv(eve_db::Ns::StarterPlexGranted)
        .get::<bool>(uid_i64, "")
        .unwrap_or(false)
    {
        return;
    }
    let Some(new_bal) = snap
        .adjust_unified_plex(char_id, plex_grant)
        .or_else(|| snap.adjust_unified_plex_for_user(user_id, plex_grant))
    else {
        return;
    };
    snap.next_entity_id += 1;
    let tx_id = snap.next_entity_id.max(1) as u64;
    crate::services::quasar_payment_store::record_plex_transaction(
        snap,
        tx_id,
        user_id,
        plex_grant,
        new_bal,
        "Starter PLEX Grant",
        now_unix,
    );
    if let Err(err) = snap
        .kv_mut(eve_db::Ns::StarterPlexGranted)
        .set_with_unix(uid_i64, "", &true, now_unix)
    {
        warn!(error = %err, "failed to persist starter_plex_granted entity_kv row");
    }
}

/// Creates each configured starter character whose account has no characters.
/// Returns the IDs of the characters created.
pub async fn provision_starter_characters(ctx: &ServerContext, cfg: &ServerConfig) -> Vec<i64> {
    let mut created = Vec::new();
    for player in &cfg.player_accounts {
        let Some(starter) = &player.starter_character else {
            continue;
        };
        let account = match ctx.db.authenticate(player.username.trim(), player.password_token.trim()) {
            Ok(a) => a,
            Err(e) => {
                warn!(username = %player.username, "starter character skipped: {e}");
                continue;
            }
        };
        let existing_chars = ctx.db.get_characters_for_user(account.user_id);
        if !existing_chars.is_empty() {
            if let Some(target_plex) = starter.initial_balance_plex {
                if target_plex > 0 {
                    let now_unix = ctx.now_unix();
                    let char_id = existing_chars[0].character_id;
                    let uid = account.user_id;
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.fold_legacy_plex_balances();
                        let cur = snap.unified_plex_balance_for_user(uid);
                        if cur < target_plex {
                            grant_starter_plex_in_snapshot(
                                snap,
                                uid,
                                char_id,
                                target_plex - cur,
                                now_unix,
                            );
                        }
                    });
                }
            }
            continue;
        }
        let mut session = SessionState::new(0);
        session.user_id = Some(account.user_id);
        session.role = account.role;
        let mut args = vec![
            PyRep::String(starter.name.clone()),
            PyRep::Integer(starter.bloodline_id),
            PyRep::Integer(starter.gender),
            PyRep::Integer(starter.ancestry_id),
            // DERIVED: a startup-created character has no paperdoll from the character creator, so
            // the appearance descriptors only carry the creation choices (no sculpt/modifier data).
            PyRep::Dict(vec![
                (PyRep::String("bloodlineID".into()), PyRep::Integer(starter.bloodline_id)),
                (PyRep::String("genderID".into()), PyRep::Integer(starter.gender)),
            ]),
            PyRep::Dict(vec![(PyRep::String("poseSelection".into()), PyRep::Integer(0))]),
            PyRep::Integer(starter.school_id),
        ];
        if let Some(sys_id) = starter.solar_system_id {
            args.push(PyRep::None);
            args.push(PyRep::Integer(sys_id));
        }
        let res = SKIP_DEFAULT_STARTER_PLEX
            .scope(
                (),
                ctx.dispatch_rpc(
                    &mut session,
                    "charUnboundMgr",
                    None,
                    "CreateCharacterWithDoll",
                    &args,
                    &HashMap::new(),
                    Some("startup"),
                ),
            )
            .await;
        match res {
            Ok(rep) => {
                let char_id = rep.as_i64().unwrap_or_default();
                if let Some(st_id) = starter.station_id {
                    let _ = ctx.db.try_with_mut_snapshot(|snap| {
                        let res = crate::router::apply_character_transition_in_snap(
                            snap,
                            &ctx.sde,
                            char_id,
                            crate::router::TransitionLocation::Station {
                                solar_system_id: None,
                                station_id: st_id,
                            },
                            None,
                            true,
                            false,
                        );
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.home_station_id = st_id;
                        }
                        res
                    });
                }
                if let Some(bal) = starter.initial_balance_isk {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = eve_db::IskCents::from_isk(bal);
                        }
                    });
                }
                let ctx_starter_plex = ctx.feature_flags_snapshot().starter_plex_balance;
                let default_plex = if ctx_starter_plex > 0 {
                    ctx_starter_plex
                } else {
                    cfg.feature_flags.starter_plex_balance
                };
                let plex_grant = starter.initial_balance_plex.unwrap_or(default_plex);
                if plex_grant > 0 {
                    let now_unix = ctx.now_unix();
                    let user_id = account.user_id;
                    ctx.db.with_mut_snapshot(|snap| {
                        grant_starter_plex_in_snapshot(
                            snap,
                            user_id,
                            char_id,
                            plex_grant,
                            now_unix,
                        );
                    });
                }
                info!(username = %player.username, char_id, "starter character created");
                created.push(char_id);
            }
            Err(e) => warn!(username = %player.username, "starter character creation failed: {} {}", e.code, e.message),
        }
    }
    created
}

#[cfg(test)]
#[path = "starter_characters_tests.rs"]
mod starter_characters_tests;

#[cfg(test)]
#[path = "starter_characters_edges2_tests.rs"]
mod starter_characters_edges2_tests;
