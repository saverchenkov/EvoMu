//! Integration tests for Equinox sovereignty hubs, startup fuel, multi-hop
//! workforce transit, Orbital Skyhooks, Mercenary Dens, and Pirate Insurgencies.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, AccountRecord, CharacterRecord, InventoryItemRecord, IskCents, NeuralAttributes,
    PersistedMercenaryDen, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_sov_hub::{all_upgrade_definitions, upgrade_definition};
use eve_server::services::quasar_sov_mercden::{
    planet_scaled_infomorphs_per_hour, simulate_den_for_planet, stage_infomorph_band,
};
use eve_server::services::quasar_sov_skyhook::planet_hourly_reagent_yield;
use eve_server::services::quasar_sov_workforce::{networkable_hubs, systems_within_jumps};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const CORP_ID: i64 = 98_000_001;
const OTHER_CORP: i64 = 98_000_002;
const PILOT_ID: i64 = 2_115_300_001;
const PILOT_SHIP: i64 = 2_115_350_001;
const HUB_JITA: i64 = 1_000_900_001;
const HUB_PERIMETER: i64 = 1_000_900_002;
const HUB_SOBAS: i64 = 1_000_900_003;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const SOBAS: u32 = 30_001_363;
const TYPE_SOV_HUB: u32 = 32_458;
const TYPE_SKYHOOK: u32 = eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK;
const TYPE_MERC_DEN: u32 = 85_230;
const TYPE_MAGMATIC_GAS: u32 = 81_143;
const STABILITY_GEN: u32 = 87_950;
const MINOR_THREAT: u32 = 82_499;

fn sess(sid: u64, cid: i64, corp: i64, sys: u32, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(cid as u64);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.solar_system_id = Some(sys);
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s.corp_role = corp_role;
    s
}

fn structure(id: i64, corp: i64, sys: u32, type_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: corp,
        solar_system_id: sys,
        name: format!("SovStructure {id}"),
        position: [150_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        hull_hp: 5_000.0,
        ..Default::default()
    }
}

async fn qrpc(ctx: &ServerContext, s: &mut SessionState, ty: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"sov-test".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-sov".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(kv) = &rep else { panic!("expected KeyVal, got {rep:?}"); };
    let raw = kv.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
        _ => None,
    }).expect("response_proto");
    EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse")
}

async fn subscribe(ctx: &ServerContext, s: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(s.session_id).expect("stream rx")
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            assert!(p.value.len() >= 2, "notice {} payload must not be empty", p.type_url);
            out.push(p.normalized_type_name().to_string());
        }
    }
    out
}

fn idm(v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, v, &mut b);
    b
}

fn id_req(field: u32, v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(field, &idm(v), &mut b);
    b
}

// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sovereignty_hub_startup_fuel_workforce_relay_and_planet_resources() {
    let db_path = std::env::temp_dir().join(format!("evomu_shard4_sov_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite open"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    common::pin_clock(&ctx);

    let jita_planet = ctx.sde.celestials_in_system(JITA).into_iter().find(|c| c.group_id == 7).map(|c| c.celestial_id).unwrap_or(40_009_080);

    db.with_mut_snapshot(|snap| {
        snap.accounts.insert("sov_dir".into(), AccountRecord {
            user_id: PILOT_ID as u64,
            username: "sov_dir".into(),
            password_token: "hash".into(),
            role: service_roles::ROLE_PLAYER_DEFAULT,
            banned_until_unix: None,
            ban_reason: None,
        });
        snap.characters.insert(PILOT_ID, CharacterRecord {
            character_id: PILOT_ID,
            user_id: PILOT_ID as u64,
            name: "EquinoxDirector".into(),
            corporation_id: CORP_ID,
            alliance_id: None,
            war_faction_id: None,
            station_id: None,
            structure_id: None,
            solar_system_id: JITA,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: PILOT_SHIP,
            balance: IskCents(10_000_000_00),
            plex_balance: 50,
            loyalty_points: HashMap::new(),
            standings: HashMap::new(),
            security_status: 5.0,
            clone_grade: 2,
            home_station_id: 60_003_760,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 11,
            school_id: 17,
            neural: NeuralAttributes::default(),
            skill_points: 5_000_000.0,
            activity_status: None,
            free_skill_points: 0.0,
            last_clone_jump_unix: 0,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: String::new(),
            title: String::new(),
            gender: 0,
            create_date_unix: 1_704_067_200,
            fleet_id: None,
            autopilot_waypoints: Vec::new(),
            version: 1,
            system_epoch: 1,
        });
        snap.corporations.entry(CORP_ID).or_default().ceo_id = PILOT_ID;
        snap.corporations.entry(CORP_ID).or_default().members.insert(PILOT_ID);
        snap.corporations.entry(CORP_ID).or_default().member_roles.insert(PILOT_ID, corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(HUB_JITA, structure(HUB_JITA, CORP_ID, JITA, TYPE_SOV_HUB));
        snap.structures.insert(HUB_PERIMETER, structure(HUB_PERIMETER, CORP_ID, PERIMETER, TYPE_SOV_HUB));
        snap.structures.insert(HUB_SOBAS, structure(HUB_SOBAS, CORP_ID, SOBAS, TYPE_SOV_HUB));
        snap.structures.insert(1_000_910_001, structure(1_000_910_001, CORP_ID, JITA, TYPE_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(1_000_910_001, jita_planet);
        snap.sov_runtime.skyhook_active.insert(1_000_910_001, true);

        for (id, ty, qty, loc) in [
            (991_001, TYPE_MAGMATIC_GAS, 200, PILOT_SHIP),
            (991_002, STABILITY_GEN, 1, HUB_JITA),
            (991_003, MINOR_THREAT, 1, HUB_JITA),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id,
                type_id: ty,
                owner_id: PILOT_ID,
                location_id: loc,
                quantity: qty,
                ..Default::default()
            });
        }
    });

    // 1. Verify startup fuel requirement on fueled upgrade definitions (2x hourly burn)
    let all_defs = all_upgrade_definitions(&ctx.sde);
    assert!(!all_defs.is_empty());
    let stab_def = upgrade_definition(&ctx.sde, STABILITY_GEN).expect("stability generator def");
    assert_eq!(stab_def.fuel_per_hour, 7);
    assert_eq!(stab_def.fuel_startup, 14);
    let unfueled_def = upgrade_definition(&ctx.sde, MINOR_THREAT).expect("minor threat def");
    assert_eq!(unfueled_def.fuel_per_hour, 0);
    assert_eq!(unfueled_def.fuel_startup, 0);

    let mut dir = sess(30, PILOT_ID, CORP_ID, JITA, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut dir).await;

    // 2. Add 100 Magmatic Gas to HUB_JITA and configure STABILITY_GEN online (consumes 14 startup fuel)
    let mut fuel_add = Vec::new();
    encode_bytes_field(1, &idm(991_001), &mut fuel_add);
    encode_varint_field(2, 100, &mut fuel_add);
    encode_bytes_field(3, &idm(HUB_JITA as u64), &mut fuel_add);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add).await.status_code, STATUS_OK);

    let mut cfg_online = Vec::new();
    let mut up_cfg = Vec::new();
    encode_bytes_field(1, &idm(u64::from(STABILITY_GEN)), &mut up_cfg);
    encode_varint_field(2, 1, &mut up_cfg);
    encode_bytes_field(1, &idm(991_002), &mut cfg_online);
    encode_bytes_field(2, &up_cfg, &mut cfg_online);
    encode_bytes_field(3, &idm(HUB_JITA as u64), &mut cfg_online);
    let up_rsp = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", cfg_online).await;
    assert_eq!(up_rsp.status_code, STATUS_OK, "ProcessConfigurationRequest failed: {}", up_rsp.status_message);

    // 100 added - 14 startup fuel = 86 remaining immediately after online transition
    let fuel_get = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_JITA as u64)).await;
    assert_eq!(fuel_get.status_code, STATUS_OK);
    let fg_wm = WireMessage::decode(&fuel_get.payload.unwrap().value).unwrap();
    let gas_rem = fg_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| {
        WireMessage::decode(f.value.as_bytes()?).ok()?.fields.iter().find(|sf| sf.field_number == 2)?.value.as_varint()
    }).unwrap();
    assert_eq!(gas_rem, 86, "startup fuel of 14 units must be deducted when upgrade comes online");

    // 3. Configure multi-hop workforce Export -> Transit relay -> Import
    let hops = systems_within_jumps(&ctx.sde, JITA, 2);
    assert!(hops.contains_key(&PERIMETER));
    db.with_snapshot(|snap| {
        let reachable = networkable_hubs(snap, &ctx.sde, HUB_JITA);
        assert!(reachable.iter().any(|&(hid, _)| hid == HUB_PERIMETER));
    });

    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(u64::from(PERIMETER)), &mut exp);
    encode_varint_field(2, 500, &mut exp);
    let mut exp_mode = Vec::new();
    encode_bytes_field(3, &exp, &mut exp_mode);
    let mut wf_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_JITA as u64), &mut wf_req);
    encode_bytes_field(2, &exp_mode, &mut wf_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req).await.status_code, STATUS_OK);

    let mut transit = Vec::new();
    encode_bytes_field(1, &idm(u64::from(JITA)), &mut transit);
    encode_bytes_field(2, &idm(u64::from(SOBAS)), &mut transit);
    let mut tr_mode = Vec::new();
    encode_bytes_field(4, &transit, &mut tr_mode);
    let mut tr_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_PERIMETER as u64), &mut tr_req);
    encode_bytes_field(2, &tr_mode, &mut tr_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", tr_req).await.status_code, STATUS_OK);

    let mut imp_src = Vec::new();
    encode_bytes_field(1, &idm(u64::from(JITA)), &mut imp_src);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(1, &imp_src, &mut imp_cfg);
    let mut imp_mode = Vec::new();
    encode_bytes_field(2, &imp_cfg, &mut imp_mode);
    let mut imp_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_SOBAS as u64), &mut imp_req);
    encode_bytes_field(2, &imp_mode, &mut imp_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", imp_req).await.status_code, STATUS_OK);
    db.with_snapshot(|snap| {
        assert_eq!(
            eve_server::services::quasar_sov_workforce::imports(snap, &ctx.sde, HUB_SOBAS),
            vec![(JITA, 500)],
        );
    });

    let planet_ver = qrpc(&ctx, &mut dir, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new()).await;
    assert_eq!(planet_ver.status_code, STATUS_OK);
    assert!(planet_ver.payload.unwrap().value.len() >= 2);
    let hub_res = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_JITA as u64)).await;
    assert_eq!(hub_res.status_code, STATUS_OK);
    assert!(hub_res.payload.unwrap().value.len() >= 2);

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n.contains("sovereignty.hub.upgrade.api.")));
    assert!(notices.iter().any(|n| n.contains("sovereignty.resource.transfer.workforce.api.ConfiguredNotice")));
    assert!(notices.iter().any(|n| n.contains("sovereignty.resource.transfer.workforce.api.StateChangedNotice")));

    drop(ctx);
    drop(db);
    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen sqlite");
    reopened.with_snapshot(|snap| {
        assert!(snap.sov_runtime.sov_hubs.contains_key(&HUB_JITA));
    });
    let _ = std::fs::remove_file(&db_path);
}

// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sovereignty_skyhook_reagent_scaling_and_mercenary_den() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let skyhook_id = 1_000_920_001i64;
    let den_id = 1_000_920_010i64;
    let mut jita_planets: Vec<i64> = ctx.sde.celestials_in_system(JITA).into_iter().filter(|c| c.group_id == 7).map(|c| c.celestial_id).collect();
    jita_planets.sort_unstable();
    let planet_a = *jita_planets.first().unwrap_or(&40_009_080);

    let y_default = planet_hourly_reagent_yield(&ctx.sde, Some(planet_a), Some(JITA), 81_143);
    assert!(y_default >= 40, "skyhook yield must reflect planet resource richness & radius");

    assert_eq!(stage_infomorph_band(1), (65, 100));
    assert_eq!(stage_infomorph_band(5), (78, 120));
    let id_rate = planet_scaled_infomorphs_per_hour(Some(&ctx.sde), JITA, planet_a, 1);
    assert!((65..=100).contains(&id_rate));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(PILOT_ID, eve_db::CharacterRecord {
            character_id: PILOT_ID,
            user_id: PILOT_ID as u64,
            name: "SkyhookDirector".into(),
            corporation_id: CORP_ID,
            alliance_id: None,
            war_faction_id: None,
            station_id: None,
            structure_id: None,
            solar_system_id: JITA,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: 1_000_920_050,
            balance: eve_db::IskCents(10_000_000_00),
            plex_balance: 0,
            loyalty_points: std::collections::HashMap::new(),
            standings: std::collections::HashMap::new(),
            security_status: 5.0,
            clone_grade: 2,
            home_station_id: 60_003_760,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 11,
            school_id: 17,
            neural: eve_db::NeuralAttributes::default(),
            skill_points: 5_000_000.0,
            activity_status: None,
            free_skill_points: 0.0,
            last_clone_jump_unix: 0,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: String::new(),
            title: String::new(),
            gender: 0,
            create_date_unix: 1_704_067_200,
            fleet_id: None,
            autopilot_waypoints: Vec::new(),
            version: 1,
            system_epoch: 1,
        });
        snap.corporations.entry(CORP_ID).or_default().ceo_id = PILOT_ID;
        snap.corporations.entry(CORP_ID).or_default().members.insert(PILOT_ID);
        snap.corporations.entry(CORP_ID).or_default().member_roles.insert(PILOT_ID, corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP_ID, JITA, TYPE_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_a);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, false);

        snap.structures.insert(den_id, structure(den_id, CORP_ID, JITA, TYPE_MERC_DEN));
        snap.sov_runtime.mercenary_dens.insert(den_id, PersistedMercenaryDen {
            den_id,
            type_id: TYPE_MERC_DEN,
            owner_char_id: PILOT_ID,
            skyhook_id,
            solar_system_id: JITA,
            planet_id: planet_a,
            enabled: true,
            cargo_extraction_enabled: false,
            development_level: 1,
            anarchy_level: 0,
            infomorphs: 0,
            simulated_at_unix: ctx.now_unix(),
            next_activity_generation_unix: ctx.now_unix(),
        });
    });

    let mut dir = sess(31, PILOT_ID, CORP_ID, JITA, corp_roles::ROLE_DIRECTOR);
    let mut outsider = sess(32, PILOT_ID + 1, OTHER_CORP, JITA, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut dir).await;

    assert_eq!(qrpc(&ctx, &mut outsider, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
    let sky_get = qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await;
    assert_eq!(sky_get.status_code, STATUS_OK);
    assert!(sky_get.payload.unwrap().value.len() >= 2);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_FORBIDDEN);

    let sky_notices = drain_notices(&mut rx);
    assert_eq!(
        sky_notices.iter().filter(|n| *n == "eve_public.sovereignty.skyhook.api.ActivationNotice").count(),
        2
    );

    let owned_dens = qrpc(&ctx, &mut dir, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await;
    assert_eq!(owned_dens.status_code, STATUS_OK);
    assert!(owned_dens.payload.unwrap().value.len() >= 2);
    let acts_rsp = qrpc(&ctx, &mut dir, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    assert_eq!(acts_rsp.status_code, STATUS_OK);
    let acts_wm = WireMessage::decode(&acts_rsp.payload.unwrap().value).unwrap();
    let act_uuid = acts_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| {
        let entry = WireMessage::decode(f.value.as_bytes()?).ok()?;
        let id_m = WireMessage::decode(entry.fields.iter().find(|ef| ef.field_number == 1)?.value.as_bytes()?).ok()?;
        id_m.fields.first()?.value.as_bytes().map(|b| b.to_vec())
    }).expect("mercenary den activity uuid");
    let mut s_act = Vec::new();
    let mut uuid_m = Vec::new();
    encode_bytes_field(1, &act_uuid, &mut uuid_m);
    encode_bytes_field(1, &uuid_m, &mut s_act);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", s_act).await.status_code, STATUS_OK);
    let den_notices = drain_notices(&mut rx);
    assert!(den_notices.iter().any(|n| n == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice"));
    assert!(den_notices.iter().any(|n| n == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));
    assert!(den_notices.iter().any(|n| n == "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice"));

    ctx.db.with_mut_snapshot(|snap| {
        let mut rem = 0u64;
        if let Some(den) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
            simulate_den_for_planet(den, &mut rem, Some(&ctx.sde), ctx.now_unix() + 7_200);
            assert!(den.infomorphs > 0);
        }
    });
}

// parity: feat:quasar.pirate
// parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_insurgency_corruption_suppression_and_space_cdn() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut player = sess(33, PILOT_ID, CORP_ID, JITA, 0);
    let mut rx = subscribe(&ctx, &mut player).await;

    eve_server::services::insurgency_services::init_insurgency_campaign(
        &ctx,
        77_001,
        500_011,
        500_001,
        JITA,
        vec![JITA],
    );
    let (c_pts, c_stage) = eve_server::services::insurgency_services::add_system_corruption(&ctx, 77_001, JITA, 250);
    assert_eq!(c_pts, 250);
    assert!(c_stage >= 1);
    let (s_pts, s_stage) = eve_server::services::insurgency_services::add_system_suppression(&ctx, 77_001, JITA, 450);
    assert_eq!(s_pts, 450);
    assert!(s_stage >= 2);

    let corr_info = qrpc(&ctx, &mut player, "eve_public.pirate.corruption.api.GetSystemInfoRequest", id_req(1, u64::from(JITA))).await;
    assert_eq!(corr_info.status_code, STATUS_OK);
    let corr_wm = WireMessage::decode(&corr_info.payload.unwrap().value).unwrap();
    let corr_stage_wire = corr_wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_varint());
    assert_eq!(corr_stage_wire, Some(u64::from(c_stage)));

    let supp_info = qrpc(&ctx, &mut player, "eve_public.pirate.suppression.api.GetSystemInfoRequest", id_req(1, u64::from(JITA))).await;
    assert_eq!(supp_info.status_code, STATUS_OK);
    let supp_wm = WireMessage::decode(&supp_info.payload.unwrap().value).unwrap();
    let supp_stage_wire = supp_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint());
    assert_eq!(supp_stage_wire, Some(u64::from(s_stage)));

    let corr_th = qrpc(&ctx, &mut player, "eve_public.pirate.corruption.api.GetStageThresholdsRequest", Vec::new()).await;
    assert_eq!(corr_th.status_code, STATUS_OK);
    let corr_th_wm = WireMessage::decode(&corr_th.payload.unwrap().value).unwrap();
    assert_eq!(corr_th_wm.fields.len(), 5);

    let supp_th = qrpc(&ctx, &mut player, "eve_public.pirate.suppression.api.GetStageThresholdsRequest", Vec::new()).await;
    assert_eq!(supp_th.status_code, STATUS_OK);
    let supp_th_wm = WireMessage::decode(&supp_th.payload.unwrap().value).unwrap();
    assert_eq!(supp_th_wm.fields.len(), 5);

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.pirate.corruption.api.GetSystemInfoRequest", id_req(1, 99_999_999)).await.status_code, STATUS_NOT_FOUND);

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n == "eve_public.pirate.corruption.api.IncreasedNotice"));
    assert!(notices.iter().any(|n| n == "eve_public.pirate.suppression.api.IncreasedNotice"));

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.space.api.cdn.GetUpdateRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    let mut cdn_req = Vec::new();
    let mut ver = Vec::new();
    encode_varint_field(1, 24, &mut ver);
    encode_varint_field(2, 1, &mut ver);
    encode_bytes_field(1, &ver, &mut cdn_req);
    encode_varint_field(2, 1, &mut cdn_req);
    let cdn_rsp = qrpc(&ctx, &mut player, "eve_public.space.api.cdn.GetUpdateRequest", cdn_req).await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    let cdn_wm = WireMessage::decode(&cdn_rsp.payload.unwrap().value).unwrap();
    let cp_bytes = cdn_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("CDN ContentPackage");
    let cp_wm = WireMessage::decode(cp_bytes).unwrap();
    let cdn_url = std::str::from_utf8(cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("CDN URL")).unwrap();
    assert!(cdn_url.contains("/maptags/v24.1."));
    assert!(cdn_url.ends_with("/universe.fb"));
}
