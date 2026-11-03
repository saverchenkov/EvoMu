//! Server-side V24 notification emitters wired to production RPCs and ticks.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

/// Emits `OnEffectMessage(msgkey, args)` (`2` args, `id_type = "charid"`).
/// Matches `eve/client/script/environment/godma.pyj:Godma.OnEffectMessage`.
pub fn emit_effect_message(
    ctx: &ServerContext,
    char_id: i64,
    msg_key: &str,
    msg_args: &[(&str, PyRep)],
) {
    let args_dict = msg_args
        .iter()
        .map(|(k, v)| (PyRep::String((*k).to_string()), v.clone()))
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnEffectMessage",
        "charid",
        vec![PyRep::String(msg_key.to_string()), PyRep::Dict(args_dict)],
    );
}

/// Emits `OnPlexDepositFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexDepositListener.OnPlexDepositFailed`.
pub fn emit_plex_deposit_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDepositFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}

/// Emits `OnPlexWithdrawFailed(error_key, reference)` (`2` args, `id_type = "charid"`).
/// Matches `vgs/common/listeners.pyj:PlexWithdrawalListener.OnPlexWithdrawFailed`.
pub fn emit_plex_withdraw_failed(
    ctx: &ServerContext,
    char_id: i64,
    error_key: &str,
    reference: PyRep,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexWithdrawFailed",
        "charid",
        vec![PyRep::String(error_key.to_string()), reference],
    );
}

/// Emits `OnServerBrainUpdated(brainData)` (`1` arg, `id_type = "charid"`).
/// Matches `eve/client/script/dogma/clientDogmaLocation.pyj:DogmaLocation.OnServerBrainUpdated`.
pub fn emit_server_brain_updated(
    ctx: &ServerContext,
    char_id: i64,
    version: i64,
    skill_type_ids: &[u32],
) {
    let mut gray_matter = Vec::with_capacity(skill_type_ids.len() * 4);
    for &tid in skill_type_ids {
        gray_matter.extend_from_slice(&tid.to_le_bytes());
    }
    let brain_data = PyRep::Tuple(vec![PyRep::Integer(version), PyRep::Buffer(gray_matter)]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerBrainUpdated",
        "charid",
        vec![brain_data],
    );
}
