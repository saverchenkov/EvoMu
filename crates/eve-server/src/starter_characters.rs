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
        if !ctx.db.get_characters_for_user(account.user_id).is_empty() {
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
        let res = ctx
            .dispatch_rpc(&mut session, "charUnboundMgr", None, "CreateCharacterWithDoll", &args,
                &HashMap::new(), Some("startup"))
            .await;
        match res {
            Ok(rep) => {
                let char_id = rep.as_i64().unwrap_or_default();
                if let Some(st_id) = starter.station_id {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.station_id = Some(st_id);
                            ch.home_station_id = st_id;
                            let ship_id = ch.active_ship_id;
                            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                                ship_item.location_id = st_id;
                            }
                        }
                    });
                }
                if let Some(bal) = starter.initial_balance_isk {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = eve_db::IskCents::from_isk(bal);
                        }
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
