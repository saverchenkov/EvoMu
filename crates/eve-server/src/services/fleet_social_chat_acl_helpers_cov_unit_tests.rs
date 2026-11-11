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

#[test]
fn chat_acl_helpers_cover_session_fallbacks_invites_operators_and_public_rooms() {
    let ctx = fresh_ctx();
    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    let char3 = 90_000_003i64;
    let char4 = 90_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char1].clone();
        ch.solar_system_id = 30_000_142;
        ch.corporation_id = 1_000_009;
        ch.alliance_id = Some(99_000_777);
        snap.characters.insert(char1, ch);

        snap.fleets.insert(
            777,
            FleetRecord {
                fleet_id: 777,
                boss_id: char1,
                members: vec![char1],
                ..Default::default()
            },
        );
    });

    // 1. validate_system_channel_resync with session.solar_system_id == None falls back to char DB
    let mut sess_none = SessionState::new(98_603);
    sess_none.character_id = Some(char1);
    assert!(validate_system_channel_resync(
        &ctx,
        &sess_none,
        char1,
        "ResyncSystemChannelAccess",
        Some("solarsystemid2:30000142"),
    )
    .is_ok());
    assert!(validate_system_channel_resync(
        &ctx,
        &sess_none,
        char1,
        "ResyncSystemChannelAccess",
        Some("resourcewars:30000142"),
    )
    .is_ok());

    // 2. validate_and_apply_channel_invite: channel:/custom DB channels, blocked_in_channel,
    // deny_list for inviter, member without privilege vs operator vs public allow_list
    let pub_chan = "channel:pub_ops".to_string();
    let custom_priv = "custom_priv_room".to_string();
    ctx.db.with_mut_snapshot(|snap| {
        snap.chat_channels.insert(
            pub_chan.clone(),
            ChatChannelRecord {
                channel_id: pub_chan.clone(),
                name: "Public Ops".into(),
                motd: String::new(),
                owner_id: Some(char1),
                members: HashSet::from([char1, char2, char3]),
                is_system_local: false,
                solar_system_id: None,
                is_private: false,
                operators: HashSet::from([char2]),
                password: None,
                is_delayed_local: false,
            },
        );
        snap.chat_channel_acl.insert(
            pub_chan.clone(),
            ChatChannelAclRecord {
                allow_list: HashSet::from([char3]),
                deny_list: HashSet::from([char4]),
                ..Default::default()
            },
        );
        snap.chat_channels.insert(
            custom_priv.clone(),
            ChatChannelRecord {
                channel_id: custom_priv.clone(),
                name: "Custom Private".into(),
                motd: String::new(),
                owner_id: Some(char1),
                members: HashSet::from([char1, char2]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: HashSet::new(),
                password: Some("pw".into()),
                is_delayed_local: false,
            },
        );
    });

    // Member char2 without invite priv on private channel is rejected
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, char2, 90_000_050, Some(&custom_priv))
            .expect_err("plain private member cannot invite")
            .code,
        "ChannelAccessDenied"
    );

    // Operator (char2) and non-private allow_list member (char3) on channel:pub_ops can invite
    validate_and_apply_channel_invite(&ctx, char2, 90_000_051, Some(&pub_chan))
        .expect("operator invite");
    validate_and_apply_channel_invite(&ctx, char3, 90_000_052, Some(&pub_chan))
        .expect("public allow_list member invite");

    // Inviter present in acl.deny_list rejected
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, char4, 90_000_053, Some(&pub_chan))
            .expect_err("inviter in deny_list rejected")
            .code,
        "ChannelAccessDenied"
    );

    // blocked_in_channel denies invite
    ctx.social_extras
        .lock()
        .unwrap()
        .blocked_in_channel
        .entry(pub_chan.clone())
        .or_default()
        .insert(90_000_055);
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, char1, 90_000_055, Some(&pub_chan))
            .expect_err("blocked recipient denied")
            .code,
        "ChannelAccessDenied"
    );

    // 3. authorize_and_fetch_channel_members DB fallbacks, org scopes, public vs passworded rooms
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [
            "corpid:1000009",
            "allianceid:99000777",
            "fleetid:777",
            "solarsystemid2:30000142",
            "constellationid:20000020",
            "regionid:10000002",
            "resourcewars:30000142",
            "public_open_room",
            "public_locked_room",
            "owner_priv_room",
            "op_priv_room",
        ] {
            let is_priv = matches!(cid, "owner_priv_room" | "op_priv_room");
            let pw = (cid == "public_locked_room").then(|| "secret".to_string());
            snap.chat_channels.insert(
                cid.into(),
                ChatChannelRecord {
                    channel_id: cid.into(),
                    name: cid.into(),
                    motd: String::new(),
                    owner_id: (cid == "owner_priv_room").then_some(char1),
                    members: HashSet::new(),
                    is_system_local: cid.starts_with("solarsystemid2:"),
                    solar_system_id: cid.starts_with("solarsystemid2:").then_some(30_000_142),
                    is_private: is_priv,
                    operators: if cid == "op_priv_room" {
                        HashSet::from([char1])
                    } else {
                        HashSet::new()
                    },
                    password: pw,
                    is_delayed_local: false,
                },
            );
        }
        snap.chat_channel_acl.insert(
            "solarsystemid2:30000142".into(),
            ChatChannelAclRecord {
                deny_list: HashSet::from([char4]),
                ..Default::default()
            },
        );
    });

    // Session has None corp/alliance/fleet -> falls back to snap.characters and snap.fleets
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "corpid:1000009").is_ok());
    assert!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "allianceid:99000777").is_ok()
    );
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "fleetid:777").is_ok());

    // Public scopes succeed, while passworded generic room without membership fails & deny_list blocks local
    assert!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "solarsystemid2:30000142")
            .is_ok()
    );
    assert!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "constellationid:20000020")
            .is_ok()
    );
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "regionid:10000002").is_ok());
    assert!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "resourcewars:30000142")
            .is_ok()
    );
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "public_open_room").is_ok());
    assert_eq!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "public_locked_room")
            .expect_err("passworded non-member room denied")
            .code,
        "ChannelAccessDenied"
    );
    assert_eq!(
        authorize_and_fetch_channel_members(&ctx, &sess_none, char4, "solarsystemid2:30000142")
            .expect_err("deny_list blocks even public system channel")
            .code,
        "ChannelAccessDenied"
    );

    // Owner/operator non-member on private channel authorized
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "owner_priv_room").is_ok());
    assert!(authorize_and_fetch_channel_members(&ctx, &sess_none, char1, "op_priv_room").is_ok());
}

#[test]
fn validate_invite_return_string_and_authorize_fetch_channel_fields_kill_mutants() {
    let ctx = fresh_ctx();
    let c1 = 90_000_001_i64;
    let c2 = 90_000_002_i64;
    let outsider = 90_000_003_i64;
    let sess = SessionState::new(98_700);

    // 1. validate_and_apply_channel_invite: exact returned channel_id (kills `Ok(String::new())` / `Ok("xyzzy".into())`)
    let priv_chan = validate_and_apply_channel_invite(&ctx, c2, c1, None)
        .expect("implicit private invite returns canonical channel id");
    assert_eq!(priv_chan, format!("private:{c1}:{c2}"));
    assert_ne!(priv_chan, "");
    assert_ne!(priv_chan, "xyzzy");

    ctx.db.with_mut_snapshot(|snap| {
        let ch = &snap.chat_channels[&priv_chan];
        assert!(ch.members.contains(&c1) && ch.members.contains(&c2));
        snap.chat_channel_acl.insert(
            priv_chan.clone(),
            ChatChannelAclRecord {
                allow_list: HashSet::from([c1, c2]),
                deny_list: HashSet::new(),
                ..Default::default()
            },
        );
    });

    // Explicit existing channel + non-member denial
    let explicit_ok = validate_and_apply_channel_invite(&ctx, c2, outsider, Some(&priv_chan))
        .expect("owner invites outsider to private channel");
    assert_eq!(explicit_ok, priv_chan);
    assert_eq!(
        validate_and_apply_channel_invite(&ctx, c1, outsider, Some(&priv_chan))
            .expect_err("non-operator member in private room cannot invite")
            .code,
        "ChannelAccessDenied"
    );

    // 2. authorize_and_fetch_channel_members: verify returned non-default (chan, acl) tuple fields
    //    kills `Ok((Default::default(), Default::default()))`.
    let (chan_rec, acl_rec) = authorize_and_fetch_channel_members(&ctx, &sess, c1, &priv_chan)
        .expect("member authorized to fetch channel details");
    assert_eq!(chan_rec.channel_id, priv_chan);
    assert_eq!(chan_rec.owner_id, Some(c2));
    assert!(chan_rec.is_private);
    assert!(chan_rec.members.contains(&outsider));
    assert!(acl_rec.allow_list.contains(&c1));
    assert!(acl_rec.allow_list.contains(&c2));
}
