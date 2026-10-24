//! Integration tests for Chat, MachoNet, and Industry Facilities RPCs:
//! - `XmppChatMgr`: `AddCharacterToChannel`, `EnsureResourceWarsChannelExists`, `GetDeprecatedPrefsFallback`,
//!   `GrantChannelOwnership`, `Hostname`, `ResyncSystemChannelAccess`
//! - `machoNet`: `ForwardCharacterNotification`, `GetCachedObject`, `GetClusterGameStatisticsForClient`,
//!   `NoBlock`, `ReloadClientCodeHash`, `SetGlobalConfigValue`
//! - `facilityManager`: `GetFacilities`, `GetFacilitiesByID`, `GetFacilityLocations`, `GetMaxActivityModifiers`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::UpwellStructureRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use crate::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const CORP_1: i64 = 98_000_001;
const JITA: u32 = 30_000_142;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(entries) => entries
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// parity: rpc:XmppChatMgr.AddCharacterToChannel
// parity: rpc:XmppChatMgr.EnsureResourceWarsChannelExists
// parity: rpc:XmppChatMgr.GetDeprecatedPrefsFallback
// parity: rpc:XmppChatMgr.GrantChannelOwnership
// parity: rpc:XmppChatMgr.Hostname
// parity: rpc:XmppChatMgr.ResyncSystemChannelAccess
// parity: rpc:machoNet.ForwardCharacterNotification
// parity: rpc:machoNet.GetCachedObject
// parity: rpc:machoNet.NoBlock
// parity: rpc:machoNet.ReloadClientCodeHash
async fn test_xmpp_chat_mgr_and_macho_net() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = login_pilot_char(&ctx, 1001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = login_pilot_char(&ctx, 1002, "other", "other-secret", PILOT_2).await;

    // 1. XmppChatMgr.Hostname: returns authentic server hostname
    let host_rep = rpc(&ctx, &mut sess1, "XmppChatMgr", "Hostname", &[])
        .await
        .expect("Hostname");
    assert_eq!(host_rep, PyRep::String("chat.evomu.local".into()));

    // 2. XmppChatMgr.GetDeprecatedPrefsFallback: returns fallback string
    let prefs_rep = rpc(&ctx, &mut sess1, "XmppChatMgr", "GetDeprecatedPrefsFallback", &[])
        .await
        .expect("GetDeprecatedPrefsFallback");
    assert_eq!(prefs_rep, PyRep::String("chat.evomu.local".into()));

    // 3. XmppChatMgr.EnsureResourceWarsChannelExists: creates authentic RW channel in snapshot
    let chan_name = "resourcewars:rw_site_999";
    let rw_rep = rpc(&ctx, &mut sess1, "XmppChatMgr", "EnsureResourceWarsChannelExists", &[PyRep::String(chan_name.into())])
        .await
        .expect("EnsureResourceWarsChannelExists");
    assert_eq!(field(&rw_rep, "channelID"), &PyRep::String(chan_name.into()));
    assert_eq!(field(&rw_rep, "exists"), &PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.chat_channels.get(chan_name).expect("resource wars channel in snapshot");
        assert!(ch.members.contains(&PILOT_1));
    });

    // 4. XmppChatMgr.AddCharacterToChannel: adds Pilot 2 to channel and emits presence notification
    let add_rep = rpc(
        &ctx,
        &mut sess1,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String(chan_name.into()), PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("AddCharacterToChannel");
    assert_eq!(field(&add_rep, "channelID"), &PyRep::String(chan_name.into()));
    assert_eq!(field(&add_rep, "characterID"), &PyRep::Integer(PILOT_2));
    assert_eq!(field(&add_rep, "joined"), &PyRep::Bool(true));
    assert_eq!(field(&add_rep, "jid"), &PyRep::String(format!("{chan_name}@chat.evomu.local")));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.chat_channels.get(chan_name).expect("channel present");
        assert!(ch.members.contains(&PILOT_2), "pilot2 added to channel members");
    });

    // Negative verification: Pilot 2 (non-owner/non-operator) cannot add arbitrary pilots
    let unauth_add = rpc(
        &ctx,
        &mut sess2,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String(chan_name.into()), PyRep::Integer(999_888)],
    )
    .await;
    assert!(unauth_add.is_err());
    assert_eq!(unauth_add.unwrap_err().code, "ChannelAccessDenied");

    // Blocked target verification: blocked pilot cannot be added by caller
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .entry(PILOT_2)
        .or_default()
        .push(eve_server::esi::runtime_state::EsiContactEntry {
            contact_id: PILOT_1,
            contact_type: "character".into(),
            standing: 0.0,
            is_watched: false,
            is_blocked: true,
            label_ids: vec![],
        });
    let blocked_add = rpc(
        &ctx,
        &mut sess1,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String(chan_name.into()), PyRep::Integer(PILOT_2)],
    )
    .await;
    assert!(blocked_add.is_err());
    assert_eq!(blocked_add.unwrap_err().code, "ChannelAccessDenied");
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .get_mut(&PILOT_2)
        .unwrap()
        .retain(|c| c.contact_id != PILOT_1);

    // 5. XmppChatMgr.GrantChannelOwnership: transfers channel ownership from Pilot 1 to Pilot 2
    let grant_rep = rpc(
        &ctx,
        &mut sess1,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[PyRep::String(chan_name.into()), PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("GrantChannelOwnership");
    assert_eq!(field(&grant_rep, "ownerID"), &PyRep::Integer(PILOT_2));
    assert_eq!(field(&grant_rep, "transferred"), &PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.chat_channels.get(chan_name).expect("channel present");
        assert_eq!(ch.owner_id, Some(PILOT_2), "pilot2 is now channel owner");
    });

    // Negative verification: Pilot 1 is no longer owner, transfer fails with NotChannelOwner
    let fail_grant = rpc(
        &ctx,
        &mut sess1,
        "XmppChatMgr",
        "GrantChannelOwnership",
        &[PyRep::String(chan_name.into()), PyRep::Integer(PILOT_1)],
    )
    .await;
    assert!(fail_grant.is_err());
    assert_eq!(fail_grant.unwrap_err().code, "NotChannelOwner");

    // 6. XmppChatMgr.ResyncSystemChannelAccess: resyncs system local channel and corp channel
    let resync_rep = rpc(&ctx, &mut sess2, "XmppChatMgr", "ResyncSystemChannelAccess", &[])
        .await
        .expect("ResyncSystemChannelAccess");
    let local_chan = field(&resync_rep, "channelID").as_str().expect("channelID str");
    assert!(local_chan.starts_with("solarsystemid2:"));
    assert_eq!(field(&resync_rep, "exists"), &PyRep::Bool(true));

    // 7. machoNet.NoBlock: non-blocking latency/keep-alive probe returns true
    let noblock_rep = rpc(&ctx, &mut sess1, "machoNet", "NoBlock", &[])
        .await
        .expect("NoBlock");
    assert_eq!(noblock_rep, PyRep::Bool(true));

    // 8. machoNet.GetCachedObject: retrieves registered cached object
    let cached_obj = rpc(&ctx, &mut sess1, "machoNet", "GetCachedObject", &[PyRep::String("config.Units".into())])
        .await
        .expect("GetCachedObject");
    match cached_obj {
        PyRep::Object { type_name, .. } => assert_eq!(type_name, "objectCaching.CachedObject"),
        other => panic!("expected objectCaching.CachedObject, got {other:?}"),
    }
    // Negative test: unknown objectID returns CachedObjectNotFound
    let unk_obj = rpc(&ctx, &mut sess1, "machoNet", "GetCachedObject", &[PyRep::String("nonexistent_obj_99".into())]).await;
    assert!(unk_obj.is_err());
    assert_eq!(unk_obj.unwrap_err().code, "CachedObjectNotFound");

    // 9. machoNet.ForwardCharacterNotification: requires GM privileges and routes notification
    let fwd_unauth = rpc(
        &ctx,
        &mut sess1,
        "machoNet",
        "ForwardCharacterNotification",
        &[PyRep::Integer(PILOT_2), PyRep::String("OnNotificationReceived".into()), PyRep::String("hello_pilot2".into())],
    )
    .await;
    assert!(fwd_unauth.is_err());
    assert_eq!(fwd_unauth.unwrap_err().code, "InsufficientRole");

    sess1.role = eve_machonet::service_roles::ROLE_GMS;
    let fwd_rep = rpc(
        &ctx,
        &mut sess1,
        "machoNet",
        "ForwardCharacterNotification",
        &[PyRep::Integer(PILOT_2), PyRep::String("OnNotificationReceived".into()), PyRep::String("hello_pilot2".into())],
    )
    .await
    .expect("ForwardCharacterNotification");
    assert_eq!(fwd_rep, PyRep::Bool(true));
    let pilot2_notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert!(pilot2_notifs.iter().any(|n| n.event == "OnNotificationReceived"));

    sess1.role = eve_machonet::service_roles::ROLE_PLAYER;

    // 10. machoNet.ReloadClientCodeHash: requires GM role
    let reload_err = rpc(&ctx, &mut sess1, "machoNet", "ReloadClientCodeHash", &[]).await;
    assert!(reload_err.is_err());
    assert_eq!(reload_err.unwrap_err().code, "InsufficientRole");

    sess1.role = eve_machonet::service_roles::ROLE_GMS;
    let reload_rep = rpc(&ctx, &mut sess1, "machoNet", "ReloadClientCodeHash", &[])
        .await
        .expect("ReloadClientCodeHash GM");
    assert_eq!(field(&reload_rep, "reloaded"), &PyRep::Bool(true));
    assert_eq!(field(&reload_rep, "hash"), &PyRep::String("v24_canonical_code_hash_verified".into()));
    sess1.role = eve_machonet::service_roles::ROLE_PLAYER;
}

#[tokio::test]
// parity: rpc:facilityManager.GetFacilities
// parity: rpc:facilityManager.GetFacilitiesByID
// parity: rpc:facilityManager.GetFacilityLocations
// parity: rpc:facilityManager.GetMaxActivityModifiers
// parity: rpc:machoNet.GetClusterGameStatisticsForClient
// parity: rpc:machoNet.SetGlobalConfigValue
async fn test_facility_manager_and_macho_stats() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = login_pilot_char(&ctx, 2001, "demo", "demo-secret", PILOT_1).await;

    // 1. machoNet.GetClusterGameStatisticsForClient: returns cluster session stats tuple
    let stats_rep = rpc(&ctx, &mut sess1, "machoNet", "GetClusterGameStatisticsForClient", &[])
        .await
        .expect("GetClusterGameStatisticsForClient");
    match stats_rep {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 3, "expected (pilotsPerSystem, dockedPerSystem, statDivisor)");
            assert_eq!(items[2], PyRep::Real(1.0));
            let PyRep::Dict(pilots) = &items[0] else { panic!("expected Dict in items[0]") };
            let pilot_count = pilots
                .iter()
                .find(|(k, _)| k.as_i64() == Some(30_000_142))
                .and_then(|(_, v)| v.as_i64())
                .unwrap_or(0);
            assert!(pilot_count >= 1, "expected at least 1 pilot in Jita");
        }
        other => panic!("expected Tuple, got {other:?}"),
    }

    // 2. machoNet.SetGlobalConfigValue: sets runtime global config value (requires GM role)
    let set_err = rpc(
        &ctx,
        &mut sess1,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("chat.xmpp.federation_enabled".into()), PyRep::Bool(true)],
    )
    .await;
    assert!(set_err.is_err());
    assert_eq!(set_err.unwrap_err().code, "InsufficientRole");

    sess1.role = eve_machonet::service_roles::ROLE_GMS;
    let set_ok = rpc(
        &ctx,
        &mut sess1,
        "machoNet",
        "SetGlobalConfigValue",
        &[PyRep::String("chat.xmpp.federation_enabled".into()), PyRep::Bool(true)],
    )
    .await
    .expect("SetGlobalConfigValue GM");
    assert_eq!(set_ok, PyRep::Bool(true));
    assert_eq!(
        eve_server::services::auth_cache::GLOBAL_CONFIG_VALUES
            .read()
            .unwrap()
            .get("chat.xmpp.federation_enabled"),
        Some(&PyRep::Bool(true))
    );
    sess1.role = eve_machonet::service_roles::ROLE_PLAYER;

    // 3. Seed an authentic Upwell engineering complex (Raitaru type 35825) in snapshot
    let struct_id = 1_000_000_000_101;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825, // Raitaru
                owner_corp_id: CORP_1,
                solar_system_id: JITA,
                name: "Jita IV Raitaru Engineering Complex".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 50000.0,
                armor_hp: 50000.0,
                hull_hp: 50000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // 4. facilityManager.GetFacilities: lists industry facilities in the system / universe
    let fac_rep = rpc(&ctx, &mut sess1, "facilityManager", "GetFacilities", &[])
        .await
        .expect("GetFacilities");
    let fac_list = match fac_rep {
        PyRep::List(list) => list,
        other => panic!("expected List, got {other:?}"),
    };
    assert!(!fac_list.is_empty(), "expected at least one facility");
    let raitaru_fac = fac_list
        .iter()
        .find(|f| field(f, "facilityID").as_i64() == Some(struct_id))
        .expect("seeded Raitaru facility found in GetFacilities");
    assert_eq!(field(raitaru_fac, "typeID"), &PyRep::Integer(35825));
    assert_eq!(field(raitaru_fac, "ownerID"), &PyRep::Integer(CORP_1));
    assert_eq!(field(raitaru_fac, "solarSystemID"), &PyRep::Integer(JITA as i64));
    assert_eq!(field(raitaru_fac, "online"), &PyRep::Bool(true));

    // 5. facilityManager.GetFacilitiesByID: retrieves specific facilities by ID list
    let by_id_rep = rpc(
        &ctx,
        &mut sess1,
        "facilityManager",
        "GetFacilitiesByID",
        &[PyRep::List(vec![PyRep::Integer(struct_id)])],
    )
    .await
    .expect("GetFacilitiesByID");
    let by_id_list = match by_id_rep {
        PyRep::List(list) => list,
        other => panic!("expected List, got {other:?}"),
    };
    assert_eq!(by_id_list.len(), 1);
    assert_eq!(field(&by_id_list[0], "facilityID"), &PyRep::Integer(struct_id));
    assert_eq!(field(&by_id_list[0], "typeID"), &PyRep::Integer(35825));

    // 6. facilityManager.GetFacilityLocations: retrieves usable hangar & corp division locations for a facility
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.corporation_id = CORP_1;
        }
        let corp = snap.corporations.entry(CORP_1).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: CORP_1,
            name: "Raitaru Corp".into(),
            ticker: "RCORP".into(),
            ceo_id: PILOT_1,
            ..Default::default()
        });
        corp.members.insert(PILOT_1);
        corp.member_roles.insert(PILOT_1, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    sess1.corporation_id = Some(CORP_1);
    sess1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let loc_rep = rpc(
        &ctx,
        &mut sess1,
        "facilityManager",
        "GetFacilityLocations",
        &[PyRep::Integer(struct_id)],
    )
    .await
    .expect("GetFacilityLocations single facility");
    let loc_list = match loc_rep {
        PyRep::List(list) => list,
        other => panic!("expected List, got {other:?}"),
    };
    assert!(
        loc_list.iter().any(|l| field(l, "ownerID") == &PyRep::Integer(PILOT_1) && field(l, "flagID") == &PyRep::Integer(4)),
        "GetFacilityLocations must include personal hangar (flagID 4)"
    );
    assert!(
        loc_list.iter().any(|l| field(l, "ownerID") == &PyRep::Integer(CORP_1) && field(l, "flagID") == &PyRep::Integer(115)),
        "GetFacilityLocations must include corp SAG divisions (flagID 115..121) when caller has hangar access"
    );

    // Unprivileged corp member without hangar roles only sees personal hangar
    let mut sess2 = login_pilot_char(&ctx, 2002, "other", "other-secret", PILOT_2).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = CORP_1;
        }
        if let Some(corp) = snap.corporations.get_mut(&CORP_1) {
            corp.members.insert(PILOT_2);
            corp.member_roles.insert(PILOT_2, 0);
        }
    });
    sess2.corporation_id = Some(CORP_1);
    sess2.corp_role = 0;
    let PyRep::List(loc_p2) = rpc(&ctx, &mut sess2, "facilityManager", "GetFacilityLocations", &[PyRep::Integer(struct_id)]).await.expect("GetFacilityLocations unprivileged") else {
        panic!("expected List");
    };
    assert!(loc_p2.iter().any(|l| field(l, "ownerID") == &PyRep::Integer(PILOT_2) && field(l, "flagID") == &PyRep::Integer(4)));
    assert!(!loc_p2.iter().any(|l| field(l, "ownerID") == &PyRep::Integer(CORP_1)), "unprivileged member must not receive corp hangar division locations");

    // 7. facilityManager.GetMaxActivityModifiers: returns maximum manufacturing/research multipliers
    let max_rep = rpc(&ctx, &mut sess1, "facilityManager", "GetMaxActivityModifiers", &[])
        .await
        .expect("GetMaxActivityModifiers");
    match field(&max_rep, "materialMultiplier") {
        PyRep::Real(r) => assert!(*r > 0.0 && *r <= 1.0),
        other => panic!("expected Real materialMultiplier, got {other:?}"),
    }
    match field(&max_rep, "timeMultiplier") {
        PyRep::Real(r) => assert!(*r > 0.0 && *r <= 1.0),
        other => panic!("expected Real timeMultiplier, got {other:?}"),
    }
    match field(&max_rep, "costMultiplier") {
        PyRep::Real(r) => assert!(*r > 0.0 && *r <= 1.0),
        other => panic!("expected Real costMultiplier, got {other:?}"),
    }
}
