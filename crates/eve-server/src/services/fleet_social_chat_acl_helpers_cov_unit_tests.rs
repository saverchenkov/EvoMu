//! Unit tests covering `fleet_social_chat_acl_helpers` (`validate_system_channel_resync`,
//! `validate_and_apply_channel_invite`, `authorize_and_fetch_channel_members`).

use super::*;
use crate::router::ServerContext;
use eve_db::{ChatChannelAclRecord, ChatChannelRecord, FleetRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn fresh_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

#[test]
fn validate_system_channel_resync_enforces_scope_and_resource_wars() {
    let ctx = fresh_ctx();
    let mut sess = SessionState::new(98_601);
    sess.solar_system_id = Some(30_000_142);

    assert!(validate_system_channel_resync(&ctx, &sess, 90_000_001, "ResyncSystemChannelAccess", None).is_ok());
    assert!(validate_system_channel_resync(
        &ctx,
        &sess,
        90_000_001,
        "EnsureResourceWarsChannelExists",
        Some("resourcewars:30000142"),
    )
    .is_ok());
    assert_eq!(
        validate_system_channel_resync(
            &ctx,
            &sess,
            90_000_001,
            "EnsureResourceWarsChannelExists",
            Some("solarsystemid2:30000142"),
        )
        .expect_err("resource wars requires resourcewars prefix")
        .code,
        "ChannelAccessDenied"
    );

    if let Some(sys) = ctx.sde.solar_systems.get(&30_000_142) {
        assert!(validate_system_channel_resync(
            &ctx,
            &sess,
            90_000_001,
            "ResyncSystemChannelAccess",
            Some(&format!("constellationid:{}", sys.constellation_id)),
        )
        .is_ok());
        assert!(validate_system_channel_resync(
            &ctx,
            &sess,
            90_000_001,
            "ResyncSystemChannelAccess",
            Some(&format!("regionid:{}", sys.region_id)),
        )
        .is_ok());
    }

    assert_eq!(
        validate_system_channel_resync(
            &ctx,
            &sess,
            90_000_001,
            "ResyncSystemChannelAccess",
            Some("solarsystemid2:30002187"),
        )
        .expect_err("foreign system rejected")
        .code,
        "ChannelAccessDenied"
    );
}

#[test]
fn channel_invite_and_authorization_enforces_blocks_acls_and_membership() {
    let ctx = fresh_ctx();
    let mut sess = SessionState::new(98_602);
    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    sess.character_id = Some(char1);
    sess.corporation_id = Some(1_000_009);
    sess.alliance_id = Some(99_000_001);
    sess.fleet_id = Some(555);

    // Creating a private channel via invite
    let created = validate_and_apply_channel_invite(&ctx, char1, char2, None).expect("private invite");
    assert_eq!(created, format!("private:{char1}:{char2}"));

    // Inviting a 3rd party by owner succeeds
    let c3 = 90_000_003i64;
    validate_and_apply_channel_invite(&ctx, char1, c3, Some(&created)).expect("owner invite");

    // Non-member invite rejected
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, 90_000_099, c3, Some(&created))
            .expect_err("non-member rejected")
            .code,
        "ChannelAccessDenied"
    );

    // Deny list blocks invite
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channel_acl.insert(
            created.clone(),
            ChatChannelAclRecord {
                deny_list: HashSet::from([90_000_004]),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, char1, 90_000_004, Some(&created))
            .expect_err("deny list blocks")
            .code,
        "ChannelAccessDenied"
    );

    // Seed public, corp, and fleet channels for authorize_and_fetch_channel_members
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channels.insert(
            "corpid:1000009".into(),
            ChatChannelRecord {
                channel_id: "corpid:1000009".into(),
                name: "Corp".into(),
                motd: String::new(),
                owner_id: None,
                members: HashSet::new(),
                is_system_local: false,
                solar_system_id: None,
                is_private: false,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
        snap.chat_channels.insert(
            "fleetid:555".into(),
            ChatChannelRecord {
                channel_id: "fleetid:555".into(),
                name: "Fleet".into(),
                motd: String::new(),
                owner_id: None,
                members: HashSet::new(),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
        snap.fleets.insert(
            555,
            FleetRecord {
                fleet_id: 555,
                boss_id: char1,
                members: vec![char1],
                ..Default::default()
            },
        );
    });

    assert!(authorize_and_fetch_channel_members(&ctx, &sess, char1, "corpid:1000009").is_ok());
    assert!(authorize_and_fetch_channel_members(&ctx, &sess, char1, "fleetid:555").is_ok());
    assert_eq!(
        authorize_and_fetch_channel_members(&ctx, &sess, char1, "channel:missing")
            .expect_err("missing channel")
            .code,
        "ChannelNotFound"
    );

    // Channel block list denies inspection
    ctx.social_extras
        .lock()
        .unwrap()
        .blocked_in_channel
        .entry(created.clone())
        .or_default()
        .insert(char1);
    assert_eq!(
        authorize_and_fetch_channel_members(&ctx, &sess, char1, &created)
            .expect_err("blocked in channel")
            .code,
        "ChannelAccessDenied"
    );
    let _ = HashMap::<String, i64>::new();
}
