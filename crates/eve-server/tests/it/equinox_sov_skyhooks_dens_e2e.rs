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
const HUB_A: i64 = 1_000_900_001;
const HUB_B: i64 = 1_000_900_002;
const HUB_C: i64 = 1_000_900_003;
const SYS_A: u32 = 30_000_669; // N3-JBX (connected sovereign null-sec)
const SYS_B: u32 = 30_000_671; // GN-PDU (1 jump from N3-JBX)
const SYS_C: u32 = 30_000_697; // 8Q-T7B (2 jumps from N3-JBX via GN-PDU)
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
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
    ]);
    let db_path = std::env::temp_dir().join(format!("evomu_shard4_sov_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite open"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    common::pin_clock(&ctx);

    let temp_planet = ctx
        .sde
        .celestials_in_system(SYS_A)
        .into_iter()
        .find(|c| c.type_id == 11)
        .map(|c| c.celestial_id)
        .unwrap_or(40_042_173);

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
            solar_system_id: SYS_A,
            constellation_id: 20_000_097,
            region_id: 10_000_007,
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
        snap.structures.insert(HUB_A, structure(HUB_A, CORP_ID, SYS_A, TYPE_SOV_HUB));
        snap.structures.insert(HUB_B, structure(HUB_B, CORP_ID, SYS_B, TYPE_SOV_HUB));
        snap.structures.insert(HUB_C, structure(HUB_C, CORP_ID, SYS_C, TYPE_SOV_HUB));
        snap.structures.insert(1_000_910_001, structure(1_000_910_001, CORP_ID, SYS_A, TYPE_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(1_000_910_001, temp_planet);
        snap.sov_runtime.skyhook_active.insert(1_000_910_001, true);

        for (id, ty, qty, loc) in [
            (991_001, TYPE_MAGMATIC_GAS, 200, PILOT_SHIP),
            (991_002, STABILITY_GEN, 1, HUB_A),
            (991_003, MINOR_THREAT, 1, HUB_A),
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

    let mut dir = sess(30, PILOT_ID, CORP_ID, SYS_A, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut dir).await;

    // 2. Add 100 Magmatic Gas to HUB_A and configure STABILITY_GEN online (consumes 14 startup fuel)
    let mut fuel_add = Vec::new();
    encode_bytes_field(1, &idm(991_001), &mut fuel_add);
    encode_varint_field(2, 100, &mut fuel_add);
    encode_bytes_field(3, &idm(HUB_A as u64), &mut fuel_add);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add).await.status_code, STATUS_OK);

    let mut cfg_online = Vec::new();
    let mut up_cfg = Vec::new();
    encode_bytes_field(1, &idm(u64::from(STABILITY_GEN)), &mut up_cfg);
    encode_varint_field(2, 1, &mut up_cfg);
    encode_bytes_field(1, &idm(991_002), &mut cfg_online);
    encode_bytes_field(2, &up_cfg, &mut cfg_online);
    encode_bytes_field(3, &idm(HUB_A as u64), &mut cfg_online);
    let up_rsp = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", cfg_online).await;
    assert_eq!(up_rsp.status_code, STATUS_OK, "ProcessConfigurationRequest failed: {}", up_rsp.status_message);

    // 100 added - 14 startup fuel = 86 remaining immediately after online transition
    let fuel_get = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(fuel_get.status_code, STATUS_OK);
    let fg_wm = WireMessage::decode(&fuel_get.payload.unwrap().value).unwrap();
    let gas_rem = fg_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| {
        WireMessage::decode(f.value.as_bytes()?).ok()?.fields.iter().find(|sf| sf.field_number == 2)?.value.as_varint()
    }).unwrap();
    assert_eq!(gas_rem, 86, "startup fuel of 14 units must be deducted when upgrade comes online");

    // 3. Configure multi-hop workforce Export -> Transit relay -> Import
    let hops = systems_within_jumps(&ctx.sde, SYS_A, 2);
    assert!(hops.contains_key(&SYS_B));
    db.with_snapshot(|snap| {
        let reachable = networkable_hubs(snap, &ctx.sde, HUB_A);
        assert!(reachable.iter().any(|&(hid, _)| hid == HUB_B));
    });

    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(u64::from(SYS_B)), &mut exp);
    encode_varint_field(2, 500, &mut exp);
    let mut exp_mode = Vec::new();
    encode_bytes_field(3, &exp, &mut exp_mode);
    let mut wf_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_A as u64), &mut wf_req);
    encode_bytes_field(2, &exp_mode, &mut wf_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req).await.status_code, STATUS_OK);

    let mut transit = Vec::new();
    encode_bytes_field(1, &idm(u64::from(SYS_A)), &mut transit);
    encode_bytes_field(2, &idm(u64::from(SYS_C)), &mut transit);
    let mut tr_mode = Vec::new();
    encode_bytes_field(4, &transit, &mut tr_mode);
    let mut tr_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_B as u64), &mut tr_req);
    encode_bytes_field(2, &tr_mode, &mut tr_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", tr_req).await.status_code, STATUS_OK);

    let mut imp_src = Vec::new();
    encode_bytes_field(1, &idm(u64::from(SYS_A)), &mut imp_src);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(1, &imp_src, &mut imp_cfg);
    let mut imp_mode = Vec::new();
    encode_bytes_field(2, &imp_cfg, &mut imp_mode);
    let mut imp_req = Vec::new();
    encode_bytes_field(1, &idm(HUB_C as u64), &mut imp_req);
    encode_bytes_field(2, &imp_mode, &mut imp_req);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", imp_req).await.status_code, STATUS_OK);
    db.with_snapshot(|snap| {
        assert_eq!(
            eve_server::services::quasar_sov_workforce::imports(snap, &ctx.sde, HUB_C),
            vec![(SYS_A, 500)],
        );
    });

    let planet_ver = qrpc(&ctx, &mut dir, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", Vec::new()).await;
    assert_eq!(planet_ver.status_code, STATUS_OK);
    assert!(planet_ver.payload.unwrap().value.len() >= 2);
    let hub_res = qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await;
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
        assert!(snap.sov_runtime.sov_hubs.contains_key(&HUB_A));
    });
    let _ = std::fs::remove_file(&db_path);
}

// parity: feat:sovereignty.skyhook-reagent-maturity
// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sovereignty_skyhook_reagent_scaling_and_mercenary_den() {
    eve_testkit::attest(&[
        "feat:sovereignty.skyhook-reagent-maturity",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let skyhook_id = 1_000_920_001i64;
    let den_id = 1_000_920_010i64;
    let sys_cels = ctx.sde.celestials_in_system(SYS_A);
    let lava_planet = sys_cels.iter().find(|c| c.type_id == 2015).map(|c| c.celestial_id).expect("N3-JBX Lava planet");
    let ice_planet = sys_cels.iter().find(|c| c.type_id == 12).map(|c| c.celestial_id).expect("N3-JBX Ice planet");
    let oceanic_planet = sys_cels.iter().find(|c| c.type_id == 2014).map(|c| c.celestial_id).expect("N3-JBX Oceanic planet");

    let y_lava = planet_hourly_reagent_yield(&ctx.sde, Some(lava_planet), Some(SYS_A), 81_143);
    assert!((520..=1_116).contains(&y_lava), "Lava yield must follow V24 curve: {y_lava}");
    let y_ice = planet_hourly_reagent_yield(&ctx.sde, Some(ice_planet), Some(SYS_A), 81_144);
    assert!((22..=46).contains(&y_ice), "Ice yield must follow V24 curve: {y_ice}");
    assert_eq!(planet_hourly_reagent_yield(&ctx.sde, Some(oceanic_planet), Some(SYS_A), 81_144), 0, "Oceanic produces 0 reagents");

    assert_eq!(stage_infomorph_band(1), (65, 100));
    assert_eq!(stage_infomorph_band(5), (78, 120));
    let id_rate = planet_scaled_infomorphs_per_hour(Some(&ctx.sde), SYS_A, lava_planet, 1);
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
            solar_system_id: SYS_A,
            constellation_id: 20_000_097,
            region_id: 10_000_007,
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
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP_ID, SYS_A, TYPE_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, lava_planet);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, false);
        snap.sov_runtime.skyhook_reagents.insert(skyhook_id, (TYPE_MAGMATIC_GAS, 0));

        snap.structures.insert(den_id, structure(den_id, CORP_ID, SYS_A, TYPE_MERC_DEN));
        snap.sov_runtime.mercenary_dens.insert(den_id, PersistedMercenaryDen {
            den_id,
            type_id: TYPE_MERC_DEN,
            owner_char_id: PILOT_ID,
            skyhook_id,
            solar_system_id: SYS_A,
            planet_id: lava_planet,
            enabled: true,
            cargo_extraction_enabled: false,
            development_level: 1,
            anarchy_level: 0,
            infomorphs: 0,
            simulated_at_unix: ctx.now_unix(),
            next_activity_generation_unix: ctx.now_unix(),
        });
    });

    let mut dir = sess(31, PILOT_ID, CORP_ID, SYS_A, corp_roles::ROLE_DIRECTOR);
    let mut outsider = sess(32, PILOT_ID + 1, OTHER_CORP, SYS_A, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut dir).await;

    assert_eq!(qrpc(&ctx, &mut outsider, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
    let sky_get = qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await;
    assert_eq!(sky_get.status_code, STATUS_OK);
    let sky_wm = WireMessage::decode(&sky_get.payload.unwrap().value).unwrap();
    assert!(sky_wm.fields.iter().all(|f| f.field_number != 6), "skyhook field 6 must not emit planet ID");
    let ver_bytes = sky_wm
        .fields
        .iter()
        .find(|f| f.field_number == 8)
        .and_then(|f| f.value.as_bytes())
        .expect("skyhook field 8 SemanticVersion");
    let ver_wm = WireMessage::decode(ver_bytes).unwrap();
    assert_eq!(ver_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1));
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
    let act_entry = acts_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| WireMessage::decode(f.value.as_bytes()?).ok()).expect("mercenary den activity entry");
    let act_uuid = WireMessage::decode(act_entry.fields.iter().find(|ef| ef.field_number == 1).unwrap().value.as_bytes().unwrap())
        .unwrap()
        .fields[0]
        .value
        .as_bytes()
        .unwrap()
        .to_vec();
    let act_attrs = WireMessage::decode(act_entry.fields.iter().find(|ef| ef.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    let act_tmpl = WireMessage::decode(act_attrs.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let dungeon_id = WireMessage::decode(act_tmpl.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap())
        .unwrap()
        .fields[0]
        .value
        .as_varint()
        .unwrap();
    assert!(matches!(dungeon_id, 12_367 | 12_368 | 12_369), "activity must use real SDE archetype-80 dungeon ID, got {dungeon_id}");
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
        let now = ctx.now_unix();
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 50, now));
        snap.sov_runtime
            .skyhook_reagents
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 150));
        snap.sov_runtime.skyhook_maturity_rem.insert(skyhook_id, (50, 123));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(skyhook_id, now);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (now, now + 3_600));
        snap.sov_runtime.skyhook_hackable.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_silo_links
            .insert(skyhook_id, (PILOT_ID, PILOT_SHIP, now + 600, 200.0));
        snap.structures.get_mut(&skyhook_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(
        qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64))
            .await
            .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64))
            .await
            .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64))
            .await
            .status_code,
        STATUS_NOT_FOUND
    );
    let local_after_destroy = qrpc(&ctx, &mut dir, "eve_public.sovereignty.skyhook.api.GetAllLocalRequest", Vec::new()).await;
    assert_eq!(local_after_destroy.status_code, STATUS_OK);
    let local_wm = WireMessage::decode(&local_after_destroy.payload.unwrap().value).unwrap();
    assert!(
        local_wm.fields.iter().all(|f| f.field_number != 2),
        "destroyed Skyhook must not be listed in GetAllLocalResponse"
    );
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_planets.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_active.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagent_bays.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_maturity_rem.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_last_harvest_unix.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_theft_windows.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_hackable.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&skyhook_id));
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
    const JITA: u32 = 30_000_142;
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
    assert!(cdn_url.contains("/maptags/v24.2."));
    assert!(cdn_url.ends_with("/universe.fb"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skyhook_live_combat_destruction_lifecycle_and_hub_reconciliation() {
    use eve_db::{PersistedHubUpgrade, PersistedSovHubData, PersistedWorkforceMode};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_server::services::quasar_sov_hub::{planet_yield_for_planet, star_power_for_system};
    use eve_server::services::quasar_sov_workforce::exported_amount;
    use eve_server::services::sov_gameplay::sov_gameplay_skyhook::{
        SKYHOOK_BASE_ARMOR_HP, SKYHOOK_BASE_HULL_HP, SKYHOOK_BASE_SHIELD_HP,
    };
    use eve_server::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure;
    use glam::DVec3;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let (sys_a, planet_id, _planet_wf, sys_b) = std::iter::once(SYS_A)
        .chain(systems_within_jumps(&ctx.sde, SYS_A, 6).into_keys())
        .filter(|&sa| star_power_for_system(&ctx.sde, sa) >= 250)
        .find_map(|sa| {
            let planet = ctx
                .sde
                .celestials_in_system_slice(sa)
                .iter()
                .filter(|c| c.group_id == 7)
                .find_map(|c| {
                    let (_, wf) = planet_yield_for_planet(c.type_id, c.radius);
                    (wf >= 3_000).then_some((c.celestial_id, wf))
                })?;
            let nb = systems_within_jumps(&ctx.sde, sa, 1)
                .into_iter()
                .find(|&(sb, hops)| {
                    hops == 1 && sb != sa && star_power_for_system(&ctx.sde, sb) >= 250
                })?
                .0;
            Some((sa, planet.0, planet.1, nb))
        })
        .expect("null-sec system pair with >=3000 workforce planet and neighbor");

    let attacker_id = PILOT_ID + 2;
    let skyhook_id = 1_000_950_001_i64;
    let hub_a = 1_000_950_010_i64;
    let hub_b = 1_000_950_011_i64;
    let export_amt = 1_500_u64;
    let pos = [200_000.0, 0.0, 0.0];

    ctx.db.with_mut_snapshot(|snap| {
        let now = ctx.now_unix();
        for (cid, corp, sys) in [
            (PILOT_ID, CORP_ID, sys_a),
            (attacker_id, OTHER_CORP, sys_a),
        ] {
            snap.characters.insert(
                cid,
                CharacterRecord {
                    character_id: cid,
                    user_id: cid as u64,
                    name: format!("Pilot {cid}"),
                    corporation_id: corp,
                    solar_system_id: sys,
                    constellation_id: 20_000_097,
                    region_id: 10_000_007,
                    active_ship_id: PILOT_SHIP + cid - PILOT_ID,
                    balance: IskCents(10_000_000_00),
                    security_status: 0.0,
                    clone_grade: 2,
                    home_station_id: 60_003_760,
                    race_id: 1,
                    bloodline_id: 1,
                    ancestry_id: 11,
                    school_id: 17,
                    neural: NeuralAttributes::default(),
                    skill_points: 5_000_000.0,
                    create_date_unix: 1_704_067_200,
                    version: 1,
                    system_epoch: 1,
                    ..Default::default()
                },
            );
            snap.corporations.entry(corp).or_default().corporation_id = corp;
            snap.corporations.entry(corp).or_default().ceo_id = cid;
            snap.corporations.entry(corp).or_default().members.insert(cid);
            snap.corporations
                .entry(corp)
                .or_default()
                .member_roles
                .insert(cid, corp_roles::ROLE_DIRECTOR);
        }

        let mut sh = structure(skyhook_id, CORP_ID, sys_a, TYPE_SKYHOOK);
        sh.position = pos;
        sh.shield_hp = SKYHOOK_BASE_SHIELD_HP;
        sh.armor_hp = SKYHOOK_BASE_ARMOR_HP;
        sh.hull_hp = SKYHOOK_BASE_HULL_HP;
        snap.structures.insert(skyhook_id, sh);
        snap.structures
            .insert(hub_a, structure(hub_a, CORP_ID, sys_a, TYPE_SOV_HUB));
        snap.structures
            .insert(hub_b, structure(hub_b, CORP_ID, sys_b, TYPE_SOV_HUB));

        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 50, now));
        snap.sov_runtime
            .skyhook_reagents
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 150));
        snap.sov_runtime.skyhook_maturity_rem.insert(skyhook_id, (25, 10));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(skyhook_id, now);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (now + 50_000, now + 57_200));
        snap.sov_runtime.skyhook_hackable.insert(skyhook_id, false);
        snap.sov_runtime
            .skyhook_silo_links
            .insert(skyhook_id, (attacker_id, PILOT_SHIP + 2, now + 600, 1_000.0));

        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: export_amt,
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![sys_a],
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: skyhook_id,
                type_id: TYPE_SKYHOOK,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_type_combat_state(TYPE_SKYHOOK),
            eve_destiny::SlimItemMeta::default(),
        )
        .await;

    let mut dir = sess(40, PILOT_ID, CORP_ID, sys_a, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut dir).await;
    drain_notices(&mut rx);

    // 1. Deplete shield outside theft window -> ArmorReinforced; further hit rejected.
    let s_hit = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        sys_a,
        attacker_id,
        skyhook_id,
        DamageVector {
            em: 20_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("shield combat hit")
    .expect("skyhook outcome");
    assert!(!s_hit.destroyed);
    assert!(s_hit.became_invulnerable);
    assert_eq!(s_hit.shield_ratio, 0.0);
    assert_eq!(s_hit.armor_ratio, 1.0);
    assert_eq!(s_hit.hull_ratio, 1.0);
    assert!(s_hit.applied_damage > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_id].state, "ArmorReinforced");
    });
    assert_eq!(
        apply_live_combat_damage_to_upwell_structure(
            &ctx,
            &sys_h,
            sys_a,
            attacker_id,
            skyhook_id,
            DamageVector {
                em: 1_000.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
            1.0,
            ctx.now_unix(),
        )
        .await
        .unwrap_err()
        .code,
        "StructureInvulnerable"
    );

    // 2. Advance past 24h reinforcement -> partial armor damage, then armor depletion -> HullVulnerable.
    ctx.advance_secs(86_400);
    let a_hit_1 = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        sys_a,
        attacker_id,
        skyhook_id,
        DamageVector {
            em: 5_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("armor hit 1")
    .expect("armor outcome 1");
    assert!(!a_hit_1.destroyed);
    assert!(!a_hit_1.became_invulnerable);
    assert_eq!(a_hit_1.shield_ratio, 0.0);
    assert!(a_hit_1.armor_ratio > 0.0 && a_hit_1.armor_ratio < 1.0);
    assert_eq!(a_hit_1.hull_ratio, 1.0);
    assert!(a_hit_1.applied_damage > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_id].state, "ArmorVulnerable");
    });

    let a_hit_2 = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        sys_a,
        attacker_id,
        skyhook_id,
        DamageVector {
            em: 50_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("armor hit 2")
    .expect("armor outcome 2");
    assert!(!a_hit_2.destroyed);
    assert_eq!(a_hit_2.armor_ratio, 0.0);
    assert_eq!(a_hit_2.hull_ratio, 1.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_id].state, "HullVulnerable");
    });

    // 3. Partial hull damage then lethal hull damage -> Destroyed + wreck + killmail + Hub/workforce cascade.
    let h_hit_1 = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        sys_a,
        attacker_id,
        skyhook_id,
        DamageVector {
            em: 4_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("hull hit 1")
    .expect("hull outcome 1");
    assert!(!h_hit_1.destroyed);
    assert_eq!(h_hit_1.armor_ratio, 0.0);
    assert!(h_hit_1.hull_ratio > 0.0 && h_hit_1.hull_ratio < 1.0);

    drain_notices(&mut rx);
    let h_kill = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        sys_a,
        attacker_id,
        skyhook_id,
        DamageVector {
            em: 50_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("lethal hull hit")
    .expect("kill outcome");
    assert!(h_kill.destroyed);
    assert!(h_kill.became_invulnerable);
    assert_eq!(h_kill.shield_ratio, 0.0);
    assert_eq!(h_kill.armor_ratio, 0.0);
    assert_eq!(h_kill.hull_ratio, 0.0);
    assert!(h_kill.applied_damage > 0.0);

    let balls = sys_h.list_balls().await.expect("list balls");
    assert!(
        !balls.iter().any(|b| b.id == skyhook_id),
        "destroyed Skyhook ball must leave space"
    );
    let expected_wreck_type = ctx
        .sde
        .resolve_wreck_type_id(TYPE_SKYHOOK, None)
        .unwrap_or(26557);
    ctx.db.with_snapshot(|snap| {
        let st = &snap.structures[&skyhook_id];
        assert_eq!(st.state, "Destroyed");
        assert_eq!(st.shield_hp, 0.0);
        assert_eq!(st.armor_hp, 0.0);
        assert_eq!(st.hull_hp, 0.0);

        assert!(!snap.sov_runtime.skyhook_planets.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_active.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagent_bays.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_maturity_rem.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_last_harvest_unix.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_theft_windows.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_hackable.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&skyhook_id));

        assert!(
            snap.killmails.values().any(|km| km.ship_type_id == TYPE_SKYHOOK
                && km.attacker_character_id == attacker_id
                && km.victim_corp_id == CORP_ID),
            "destroyed Skyhook must emit a killmail"
        );
        let wreck_item = snap
            .items
            .values()
            .find(|it| it.type_id == expected_wreck_type && it.location_id == i64::from(sys_a))
            .expect("Skyhook wreck item spawned in system");
        assert!(
            balls.iter().any(|b| b.id == wreck_item.item_id),
            "Skyhook wreck ball must exist in space"
        );

        assert!(snap.sov_runtime.sov_hubs[&hub_a].upgrades[0].low_power);
        assert_eq!(exported_amount(snap, hub_a), 0);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });

    let notices = drain_notices(&mut rx);
    assert!(notices
        .iter()
        .any(|n| n == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice"));
    assert!(notices
        .iter()
        .any(|n| n == "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice"));
    assert!(notices
        .iter()
        .any(|n| n == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice"));

    assert_eq!(
        apply_live_combat_damage_to_upwell_structure(
            &ctx,
            &sys_h,
            sys_a,
            attacker_id,
            skyhook_id,
            DamageVector {
                em: 1_000.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
            1.0,
            ctx.now_unix(),
        )
        .await
        .unwrap_err()
        .code,
        "StructureAlreadyDestroyed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_workforce_mecha_tooling_anti_duplication_and_export_cascade() {
    use eve_db::{
        CharacterRecord, InventoryFlag, InventoryItemRecord, PersistedMercenaryActivity,
        PersistedMercenaryDen, PersistedSovHubData, PersistedWorkforceMode,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::service_roles;
    use eve_server::services::quasar_sov_hub::{
        compute_hub_resources, planet_yield_for_planet,
    };
    use eve_server::services::quasar_sov_workforce::{
        encode_configuration, exported_amount, imports,
    };
    use eve_server::services::sov_gameplay::sov_gameplay_skyhook::{
        SKYHOOK_BASE_ARMOR_HP, SKYHOOK_BASE_HULL_HP, SKYHOOK_BASE_SHIELD_HP,
    };
    use eve_server::services::sov_gameplay::{
        apply_mercenary_den_shield_repair, apply_mercenary_den_weapon_damage, MERC_DEN_BASE_HP,
    };
    use eve_server::services::sov_services::TYPE_SUPERIONIC_ICE;
    use glam::DVec3;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let t0 = ctx.now_unix();

    let temp_cel = ctx
        .sde
        .celestials_in_system(SYS_A)
        .into_iter()
        .find(|c| c.type_id == 11)
        .expect("N3-JBX Temperate planet");
    let gas_cel_a = ctx
        .sde
        .celestials_in_system(SYS_A)
        .into_iter()
        .filter(|c| c.type_id == 13)
        .max_by_key(|c| planet_yield_for_planet(c.type_id, c.radius).0)
        .expect("N3-JBX Gas planet");
    let gas_cel_b = ctx
        .sde
        .celestials_in_system(SYS_B)
        .into_iter()
        .filter(|c| c.type_id == 13)
        .max_by_key(|c| planet_yield_for_planet(c.type_id, c.radius).0)
        .expect("GN-PDU Gas planet");

    let wf_base = planet_yield_for_planet(temp_cel.type_id, temp_cel.radius).1;
    assert!(wf_base >= 5_500, "expected >= 5_500 N3-JBX Temperate workforce, got {wf_base}");

    let skyhook_temp = 1_000_950_001_i64;
    let skyhook_gas_a = 1_000_950_002_i64;
    let skyhook_gas_b = 1_000_950_003_i64;
    let den_id = 1_000_950_010_i64;
    let mto_ball_id = 1_000_950_020_i64;
    let act_uuid = vec![0xE2_u8; 16];
    let act_key: String = act_uuid.iter().map(|b| format!("{b:02x}")).collect();

    let item_tooling = 995_001_i64;
    let item_stab400 = 995_002_i64;
    let item_threat2_b = 995_003_i64;
    let item_ice = 995_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "e2e_wf_dir".into(),
            eve_db::AccountRecord {
                user_id: PILOT_ID as u64,
                username: "e2e_wf_dir".into(),
                password_token: "hash".into(),
                role: service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_ADMIN,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(
            PILOT_ID,
            CharacterRecord {
                character_id: PILOT_ID,
                user_id: PILOT_ID as u64,
                name: "EquinoxWorkforceDir".into(),
                corporation_id: CORP_ID,
                solar_system_id: SYS_A,
                active_ship_id: PILOT_SHIP,
                security_status: 5.0,
                ..Default::default()
            },
        );
        let corp = snap.corporations.entry(CORP_ID).or_default();
        corp.corporation_id = CORP_ID;
        corp.ceo_id = PILOT_ID;
        corp.members.insert(PILOT_ID);
        corp.member_roles.insert(PILOT_ID, corp_roles::ROLE_DIRECTOR);

        for (hid, sys) in [(HUB_A, SYS_A), (HUB_B, SYS_B)] {
            snap.structures.insert(hid, structure(hid, CORP_ID, sys, TYPE_SOV_HUB));
            snap.sov_runtime.sov_hubs.insert(
                hid,
                PersistedSovHubData {
                    hub_id: hid,
                    last_updated_unix: t0,
                    ..Default::default()
                },
            );
        }
        for (sh_id, sys, cel_id) in [
            (skyhook_temp, SYS_A, temp_cel.celestial_id),
            (skyhook_gas_a, SYS_A, gas_cel_a.celestial_id),
            (skyhook_gas_b, SYS_B, gas_cel_b.celestial_id),
        ] {
            let mut sh = structure(sh_id, CORP_ID, sys, TYPE_SKYHOOK);
            sh.state = "Online".into();
            sh.shield_hp = SKYHOOK_BASE_SHIELD_HP;
            sh.armor_hp = SKYHOOK_BASE_ARMOR_HP;
            sh.hull_hp = SKYHOOK_BASE_HULL_HP;
            snap.structures.insert(sh_id, sh);
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel_id);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }

        let mut den_st = structure(den_id, CORP_ID, SYS_A, TYPE_MERC_DEN);
        den_st.state = "ShieldVulnerable".into();
        den_st.shield_hp = MERC_DEN_BASE_HP;
        den_st.armor_hp = MERC_DEN_BASE_HP;
        den_st.hull_hp = MERC_DEN_BASE_HP;
        snap.structures.insert(den_id, den_st);
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_temp, 10);
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: PILOT_ID,
                skyhook_id: skyhook_temp,
                solar_system_id: SYS_A,
                planet_id: temp_cel.celestial_id,
                type_id: TYPE_MERC_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 10,
                anarchy_level: 0,
                infomorphs: 50,
                simulated_at_unix: t0,
                next_activity_generation_unix: t0 + 86_400,
            },
        );
        snap.sov_runtime.mercenary_activities.insert(
            act_key.clone(),
            PersistedMercenaryActivity {
                uuid: act_uuid.clone(),
                den_id,
                solar_system_id: SYS_A,
                dungeon_id: 12_367,
                started: true,
                started_by: PILOT_ID,
                expiry_unix: t0 + 86_400,
                development_impact: 5,
                anarchy_impact: -5,
                infomorph_bonus: 0,
            },
        );
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id, act_key.clone());
        snap.items.insert(
            mto_ball_id,
            InventoryItemRecord {
                item_id: mto_ball_id,
                type_id: 34,
                owner_id: PILOT_ID,
                location_id: i64::from(SYS_A),
                quantity: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(mto_ball_id, [100.0, 0.0, 0.0]);

        for (id, ty, qty, loc) in [
            (item_tooling, 87_710_u32, 1_i32, HUB_A), // Mecha-Tooling 1: 200p, +5_000w, 8 Ice/hr
            (item_stab400, 87_948_u32, 1_i32, HUB_A), // Electric Stability 1: 400p, -4_000w, unfueled
            (item_threat2_b, 82_500_u32, 1_i32, HUB_B), // Minor Threat 2: 500p, -3_800w, unfueled
            (item_ice, TYPE_SUPERIONIC_ICE, 5_000_i32, HUB_A),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: ty,
                    owner_id: PILOT_ID,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });

    let sys_h = ctx.systems.get_or_spawn(SYS_A);
    for (bid, ty) in [
        (PILOT_SHIP, 670_u32),
        (skyhook_temp, TYPE_SKYHOOK),
        (skyhook_gas_a, TYPE_SKYHOOK),
        (den_id, TYPE_MERC_DEN),
        (mto_ball_id, 34_u32),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: bid,
                type_id: ty,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    let mut dir = sess(31, PILOT_ID, CORP_ID, SYS_A, corp_roles::ROLE_DIRECTOR);
    dir.role |= service_roles::ROLE_ADMIN;
    let mut rx = subscribe(&ctx, &mut dir).await;

    let encode_cfg_req = |hub_id: i64, mode: &PersistedWorkforceMode| {
        let mut req = id_req(1, hub_id as u64);
        encode_bytes_field(2, &encode_configuration(mode), &mut req);
        req
    };
    let encode_proc_req = |hub_id: i64, new_items: &[i64], cfgs: &[(u32, bool)]| {
        let mut req = Vec::new();
        for &item_id in new_items {
            let mut nu = Vec::new();
            encode_varint_field(1, item_id as u64, &mut nu);
            encode_bytes_field(1, &nu, &mut req);
        }
        for &(type_id, online) in cfgs {
            let mut ce = id_req(1, u64::from(type_id));
            if online {
                encode_varint_field(2, 1, &mut ce);
            }
            encode_bytes_field(2, &ce, &mut req);
        }
        encode_bytes_field(3, &idm(hub_id as u64), &mut req);
        req
    };

    // 1. Add Superionic Ice and install/online [87_710 (+5_000w), 87_948 (-4_000w)] on HUB_A.
    let mut fuel_req = id_req(1, item_ice as u64);
    encode_varint_field(2, 4_000, &mut fuel_req);
    encode_bytes_field(3, &idm(HUB_A as u64), &mut fuel_req);
    assert_eq!(
        qrpc(&ctx, &mut dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_A, &[item_tooling, item_stab400], &[(87_710, true), (87_948, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );

    // 2. Anti-duplication check: exporting above wf_base (trying to export 87_710 output) must fail!
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_A,
                &PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: wf_base + 500,
                },
            ),
        )
        .await
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 3. Configure valid Export 5_000 HUB_A -> HUB_B (running 82_500 requiring 3_800w).
    let expected_stage5_wf = wf_base - (wf_base * 40) / 100;
    let expected_no_tooling_wf = wf_base - 4_000;
    assert!(wf_base >= 5_000);
    assert!(expected_stage5_wf < 3_800, "Stage 5 -40% {expected_stage5_wf} must starve HUB_B 82_500");
    assert!(
        expected_no_tooling_wf < 3_800,
        "offlining 87_710 leaves {expected_no_tooling_wf} < 3_800 on HUB_B"
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_A,
                &PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: 5_000,
                },
            ),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_B,
                &PersistedWorkforceMode::Import {
                    sources: vec![SYS_A],
                },
            ),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_B, &[item_threat2_b], &[(82_500, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );

    // 4. ProcessConfigurationRequest offlining 87_710 on HUB_A clamps Export -> wf_base - 4_000 & drops HUB_B.
    let _ = drain_notices(&mut rx);
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_A, &[], &[(87_710, false)]),
        )
        .await
        .status_code,
        STATUS_OK
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, HUB_A), expected_no_tooling_wf);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, expected_no_tooling_wf)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
    });
    let offline_notices = drain_notices(&mut rx);
    assert!(offline_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(offline_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // Restore 87_710 online, Export 5_000, HUB_B 82_500 online.
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_A, &[], &[(87_710, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_A,
                &PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: 5_000,
                },
            ),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_B, &[], &[(82_500, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );

    // 5. GM ModifyAnarchyLevelRequest -> Stage 5 (-40%): HUB_A clamps Export -> expected_stage5_wf & HUB_B drops.
    let _ = drain_notices(&mut rx);
    let mut mod_req = id_req(1, den_id as u64);
    encode_varint_field(2, 100, &mut mod_req);
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
            mod_req,
        )
        .await
        .status_code,
        STATUS_OK
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            compute_hub_resources(snap, &ctx.sde, HUB_A).unwrap().workforce_local,
            expected_stage5_wf
        );
        assert_eq!(exported_amount(snap, HUB_A), expected_stage5_wf);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
    });
    let mod_notices = drain_notices(&mut rx);
    assert!(mod_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(mod_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // 6. Live Mercenary Den shield depletion disables Den (disruption 40% -> 0%), restoring wf_base;
    //    then restoring Export 5_000 + HUB_B, shield repair >=50% re-enables Stage 5 disruption & cascades!
    let _ = drain_notices(&mut rx);
    apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .expect("deplete Den shields to disable it");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.mercenary_dens[&den_id].enabled);
        assert_eq!(
            compute_hub_resources(snap, &ctx.sde, HUB_A).unwrap().workforce_local,
            wf_base
        );
    });
    assert!(drain_notices(&mut rx)
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_A,
                &PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: 5_000,
                },
            ),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_B, &[], &[(82_500, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );

    let den_reinf_until = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_den_reinforcement
            .get(&den_id)
            .map(|r| r.0)
            .unwrap_or(0)
    });
    if den_reinf_until > ctx.now_unix() {
        ctx.advance_secs(den_reinf_until - ctx.now_unix() + 1);
    }
    let _ = drain_notices(&mut rx);
    let (_pct, re_enabled) =
        apply_mercenary_den_shield_repair(&ctx, SYS_A, PILOT_SHIP, den_id, MERC_DEN_BASE_HP)
            .await
            .expect("repair Den >= 50% re-enables Stage 5 disruption");
    assert!(re_enabled);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.sov_runtime.mercenary_dens[&den_id].enabled);
        assert_eq!(exported_amount(snap, HUB_A), expected_stage5_wf);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
    });
    let rep_notices = drain_notices(&mut rx);
    assert!(rep_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(rep_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // 7. Re-enable 0% Anarchy via Disable/Modify, restore Export 5_000 + HUB_B, then sovMgr.DestroySkyhooks
    //    via dispatch_rpc destroys both Skyhooks + Den + MTO ball, clamps Export -> 0, drops HUB_B, & flushes notices!
    let mut mod_zero = id_req(1, den_id as u64);
    encode_varint_field(2, 0, &mut mod_zero);
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
            mod_zero,
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            encode_cfg_req(
                HUB_A,
                &PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: 5_000,
                },
            ),
        )
        .await
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        qrpc(
            &ctx,
            &mut dir,
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            encode_proc_req(HUB_B, &[], &[(82_500, true)]),
        )
        .await
        .status_code,
        STATUS_OK
    );

    let _ = drain_notices(&mut rx);
    let destroyed = ctx
        .dispatch_rpc(
            &mut dir,
            "sovMgr",
            None,
            "DestroySkyhooks",
            &[PyRep::List(vec![
                PyRep::Integer(skyhook_temp),
                PyRep::Integer(skyhook_gas_a),
            ])],
            &Default::default(),
            None,
        )
        .await
        .expect("DestroySkyhooks E2E");
    assert_eq!(destroyed, PyRep::Integer(2));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&skyhook_temp));
        assert!(!snap.structures.contains_key(&skyhook_gas_a));
        assert_eq!(snap.structures[&den_id].state, "Destroyed");
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_id));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&act_key));
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_id));
        assert!(!snap.sov_runtime.mercenary_den_tax_pct.contains_key(&skyhook_temp));
        assert_eq!(exported_amount(snap, HUB_A), 0);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
    });
    let (has_skyhook, has_den, has_mto) = sys_h
        .test_with_ballpark_async(move |bp| {
            (
                bp.balls.contains_key(&skyhook_temp),
                bp.balls.contains_key(&den_id),
                bp.balls.contains_key(&mto_ball_id),
            )
        })
        .await;
    assert!(!has_skyhook);
    assert!(!has_den);
    assert!(!has_mto);
    let destroy_notices = drain_notices(&mut rx);
    assert!(destroy_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(destroy_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_skyhook_zero_shield_in_theft_window_reinforces_and_destructs_or_repairs() {
    use eve_db::{
        CharacterRecord, PersistedHubUpgrade, PersistedSovHubData, PersistedWorkforceMode,
    };
    use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
    use eve_server::services::quasar_sov_hub::planet_yield_for_planet;
    use eve_server::services::quasar_sov_workforce::{exported_amount, imports};
    use eve_server::services::sov_gameplay::sov_gameplay_skyhook::{
        tick_skyhooks, SKYHOOK_BASE_ARMOR_HP, SKYHOOK_BASE_HULL_HP, SKYHOOK_BASE_SHIELD_HP,
    };
    use eve_server::services::station_structure::structure_lifecycle::{
        REPAIR_HULL_SECS, REPAIR_SHIELD_ARMOR_SECS,
    };
    use eve_server::services::station_structure::structure_lifecycle_rpc::apply_live_combat_damage_to_upwell_structure;
    use glam::DVec3;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let t0 = ctx.now_unix();

    let temp_cel = ctx
        .sde
        .celestials_in_system(SYS_A)
        .into_iter()
        .find(|c| c.type_id == 11)
        .expect("N3-JBX Temperate planet");
    let lava_cel = ctx
        .sde
        .celestials_in_system(SYS_A)
        .into_iter()
        .find(|c| c.type_id == 2015)
        .expect("N3-JBX Lava planet");
    let gas_cel_b = ctx
        .sde
        .celestials_in_system(SYS_B)
        .into_iter()
        .filter(|c| c.type_id == 13)
        .max_by_key(|c| planet_yield_for_planet(c.type_id, c.radius).0)
        .expect("GN-PDU Gas planet");

    let skyhook_a = 1_000_960_001_i64;
    let skyhook_b = 1_000_960_002_i64;
    let skyhook_gas_b = 1_000_960_003_i64;
    let attacker_id = 2_115_399_001_i64;
    let attacker_ship = 2_115_399_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            PILOT_ID,
            CharacterRecord {
                character_id: PILOT_ID,
                user_id: PILOT_ID as u64,
                name: "SkyhookOwner".into(),
                corporation_id: CORP_ID,
                solar_system_id: SYS_A,
                active_ship_id: PILOT_SHIP,
                ..Default::default()
            },
        );
        snap.characters.insert(
            attacker_id,
            CharacterRecord {
                character_id: attacker_id,
                user_id: attacker_id as u64,
                name: "SkyhookAttacker".into(),
                corporation_id: OTHER_CORP,
                solar_system_id: SYS_A,
                active_ship_id: attacker_ship,
                ..Default::default()
            },
        );
        snap.structures.insert(HUB_A, structure(HUB_A, CORP_ID, SYS_A, TYPE_SOV_HUB));
        snap.structures.insert(HUB_B, structure(HUB_B, CORP_ID, SYS_B, TYPE_SOV_HUB));

        let mut sh_a = structure(skyhook_a, CORP_ID, SYS_A, TYPE_SKYHOOK);
        sh_a.state = "ShieldVulnerable".into();
        sh_a.shield_hp = SKYHOOK_BASE_SHIELD_HP;
        sh_a.armor_hp = SKYHOOK_BASE_ARMOR_HP;
        sh_a.hull_hp = SKYHOOK_BASE_HULL_HP;
        snap.structures.insert(skyhook_a, sh_a);
        snap.sov_runtime.skyhook_planets.insert(skyhook_a, temp_cel.celestial_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_a, true);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_a, (t0, t0 + 3_600));

        let mut sh_gb = structure(skyhook_gas_b, CORP_ID, SYS_B, TYPE_SKYHOOK);
        sh_gb.state = "Online".into();
        sh_gb.shield_hp = SKYHOOK_BASE_SHIELD_HP;
        sh_gb.armor_hp = SKYHOOK_BASE_ARMOR_HP;
        sh_gb.hull_hp = SKYHOOK_BASE_HULL_HP;
        snap.structures.insert(skyhook_gas_b, sh_gb);
        snap.sov_runtime
            .skyhook_planets
            .insert(skyhook_gas_b, gas_cel_b.celestial_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_gas_b, true);

        snap.sov_runtime.sov_hubs.insert(
            HUB_A,
            PersistedSovHubData {
                hub_id: HUB_A,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: 5_000,
                },
                last_updated_unix: t0,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            HUB_B,
            PersistedSovHubData {
                hub_id: HUB_B,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![SYS_A],
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 82_500, // 500p, 3_800w
                    online: true,
                    low_power: false,
                }],
                last_updated_unix: t0,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(SYS_A);
    for (bid, ty) in [
        (attacker_ship, 670_u32),
        (skyhook_a, TYPE_SKYHOOK),
        (skyhook_b, TYPE_SKYHOOK),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: bid,
                type_id: ty,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    let mut owner_sess = sess(41, PILOT_ID, CORP_ID, SYS_A, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut owner_sess).await;

    let heavy = DamageVector {
        em: 50_000.0,
        thermal: 50_000.0,
        kinetic: 50_000.0,
        explosive: 50_000.0,
    };

    // 1. Deplete shield to 0.0 inside open theft window -> remains ShieldVulnerable.
    ctx.advance_secs(3_500);
    let hit1 = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        SYS_A,
        attacker_id,
        skyhook_a,
        heavy,
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("hit during theft window")
    .expect("outcome");
    assert!(!hit1.became_invulnerable);
    ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&skyhook_a];
        assert_eq!(s.state, "ShieldVulnerable");
        assert_eq!(s.shield_hp, 0.0);
        assert_eq!(s.armor_hp, SKYHOOK_BASE_ARMOR_HP);
    });

    // 2. Window closes (+101s, only 101s < 900s repair) -> live attack on 0.0-shield skyhook
    //    MUST transition to ArmorReinforced (never permanently invulnerable!).
    ctx.advance_secs(101);
    tick_skyhooks(&ctx, ctx.now_unix()).await;
    let hit2 = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        SYS_A,
        attacker_id,
        skyhook_a,
        heavy,
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("post-window hit on zero-shield skyhook")
    .expect("outcome");
    assert!(hit2.became_invulnerable);
    let reinf_until = ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&skyhook_a];
        assert_eq!(s.state, "ArmorReinforced");
        assert!(s.reinforced_until_unix > ctx.now_unix());
        s.reinforced_until_unix
    });

    // Negative guard during reinforcement rejects damage.
    assert_eq!(
        apply_live_combat_damage_to_upwell_structure(
            &ctx,
            &sys_h,
            SYS_A,
            attacker_id,
            skyhook_a,
            heavy,
            1.0,
            ctx.now_unix(),
        )
        .await
        .unwrap_err()
        .code,
        "StructureInvulnerable"
    );

    // 3. Exit ArmorReinforced -> deplete armor -> HullVulnerable -> deplete hull -> Destroyed + reconcile hub.
    ctx.advance_secs(reinf_until - ctx.now_unix() + 1);
    tick_skyhooks(&ctx, ctx.now_unix()).await;
    let _ = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        SYS_A,
        attacker_id,
        skyhook_a,
        heavy,
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("deplete armor")
    .expect("outcome");
    let _ = drain_notices(&mut rx);
    let kill = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        SYS_A,
        attacker_id,
        skyhook_a,
        heavy,
        1.0,
        ctx.now_unix(),
    )
    .await
    .expect("deplete hull")
    .expect("outcome");
    assert!(kill.destroyed);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_a].state, "Destroyed");
        assert_eq!(exported_amount(snap, HUB_A), 0);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
    });
    let dest_notices = drain_notices(&mut rx);
    assert!(dest_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(dest_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // 4. Unattacked Skyhook in HullVulnerable requires REPAIR_HULL_SECS (1_800s), does NOT repair
    //    at 900s, then restores full ShieldVulnerable HP & syncs Ballpark CombatState at 1_800s.
    let t_b = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut sh_b = structure(skyhook_b, CORP_ID, SYS_A, TYPE_SKYHOOK);
        sh_b.state = "HullVulnerable".into();
        sh_b.shield_hp = 0.0;
        sh_b.armor_hp = 0.0;
        sh_b.hull_hp = 4_000.0;
        sh_b.last_damage_unix = t_b;
        snap.structures.insert(skyhook_b, sh_b);
        snap.sov_runtime.skyhook_planets.insert(skyhook_b, lava_cel.celestial_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_b, true);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_b, (t_b + 5_000, t_b + 12_200));
    });
    sys_h
        .test_with_ballpark_mut_async(move |bp| {
            bp.combat_states.insert(
                skyhook_b,
                CombatState {
                    shield_hp: 0.0,
                    max_shield_hp: SKYHOOK_BASE_SHIELD_HP,
                    armor_hp: 0.0,
                    max_armor_hp: SKYHOOK_BASE_ARMOR_HP,
                    structure_hp: 4_000.0,
                    max_structure_hp: SKYHOOK_BASE_HULL_HP,
                    ..Default::default()
                },
            );
        })
        .await;

    ctx.advance_secs(REPAIR_SHIELD_ARMOR_SECS);
    tick_skyhooks(&ctx, ctx.now_unix()).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_b].state, "HullVulnerable");
        assert_eq!(snap.structures[&skyhook_b].hull_hp, 4_000.0);
    });

    ctx.advance_secs(REPAIR_HULL_SECS - REPAIR_SHIELD_ARMOR_SECS);
    tick_skyhooks(&ctx, ctx.now_unix()).await;
    ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&skyhook_b];
        assert_eq!(s.state, "ShieldVulnerable");
        assert_eq!(s.shield_hp, SKYHOOK_BASE_SHIELD_HP);
        assert_eq!(s.armor_hp, SKYHOOK_BASE_ARMOR_HP);
        assert_eq!(s.hull_hp, SKYHOOK_BASE_HULL_HP);
        assert_eq!(s.last_damage_unix, 0);
    });
    let (shield_hp, armor_hp, structure_hp) = sys_h
        .test_with_ballpark_async(move |bp| {
            let cs = &bp.combat_states[&skyhook_b];
            (cs.shield_hp, cs.armor_hp, cs.structure_hp)
        })
        .await;
    assert_eq!(shield_hp, SKYHOOK_BASE_SHIELD_HP);
    assert_eq!(armor_hp, SKYHOOK_BASE_ARMOR_HP);
    assert_eq!(structure_hp, SKYHOOK_BASE_HULL_HP);
}

// parity: rpc:colonyResourcesMgr.GetMyCorporationSkyhooks
// parity: rpc:map.GetCurrentSovData
// parity: rpc:sovMgr.DestroySkyhooks
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_skyhook_tombstone_mercden_vuln_expiry_and_combat_hub_cleanup() {
    eve_testkit::attest(&[
        "rpc:colonyResourcesMgr.GetMyCorporationSkyhooks",
        "rpc:map.GetCurrentSovData",
        "rpc:sovMgr.DestroySkyhooks",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
    ]);

    use eve_db::durable_domain_types::{
        PersistedHubUpgrade, PersistedMercenaryActivity, PersistedSovHubData,
    };
    use eve_db::{
        AllianceRecord, CorporationRecord, OnlineSessionRecord, PersistedWorkforceMode,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_server::services::quasar_sov_hub::{
        all_upgrade_definitions, compute_hub_resources, planet_yield_for_planet, system_yield,
        UpgradeDefinition,
    };
    use eve_server::services::quasar_sov_mercden::anarchy_workforce_disruption_pct;
    use eve_server::services::quasar_sov_workforce::{exported_amount, imports};
    use eve_server::services::sov_gameplay::sov_gameplay_skyhook::{
        SKYHOOK_BASE_ARMOR_HP, SKYHOOK_BASE_HULL_HP, SKYHOOK_BASE_SHIELD_HP,
    };
    use eve_server::services::sov_gameplay::tick_sov_gameplay;
    use eve_server::services::station_structure::structure_lifecycle::{
        destroy_structure, flush_disposal_quasar_notices, STATE_DESTROYED,
    };
    use eve_server::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state,
    };
    use glam::DVec3;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let alliance_id: i64 = 99_900_999;
    let dir_b_id: i64 = PILOT_ID + 1;
    let den_owner: i64 = 2_115_300_099;
    let old_hub_a: i64 = 1_000_899_001;
    let skyhook_a: i64 = 1_000_910_101;
    let skyhook_b: i64 = 1_000_910_102;
    let skyhook_new: i64 = 1_000_910_103;
    let den_a: i64 = 1_000_920_101;
    let den_b: i64 = 1_000_920_102;
    let den_new: i64 = 1_000_920_103;
    let mto_ball_a: i64 = 1_990_920_101;
    let now = ctx.now_unix();

    let (planet_wf_id, planet_wf) = ctx
        .sde
        .celestials_in_system_slice(SYS_A)
        .iter()
        .filter(|c| c.group_id == 7)
        .map(|c| (c.celestial_id, planet_yield_for_planet(c.type_id, c.radius).1))
        .max_by_key(|&(_, wf)| wf)
        .expect("SYS_A workforce planet");
    assert!(planet_wf > 0);
    let (planet_2, planet_2_wf) = ctx
        .sde
        .celestials_in_system_slice(SYS_A)
        .iter()
        .filter(|c| c.group_id == 7 && c.celestial_id != planet_wf_id)
        .map(|c| (c.celestial_id, planet_yield_for_planet(c.type_id, c.radius).1))
        .min_by_key(|&(_, wf)| wf)
        .expect("SYS_A secondary planet");
    assert_eq!(planet_2_wf, 0, "expected secondary planet to contribute zero workforce");

    let sys_a_power: u64 = system_yield(&ctx.sde, SYS_A).stars.iter().map(|(_, p)| *p).sum();
    let sys_b_power: u64 = system_yield(&ctx.sde, SYS_B).stars.iter().map(|(_, p)| *p).sum();
    let disrupted_planet =
        planet_wf - (planet_wf * anarchy_workforce_disruption_pct(5)) / 100;
    let defs: Vec<UpgradeDefinition> = all_upgrade_definitions(&ctx.sde);
    let hub_a_local = defs
        .iter()
        .find(|d| {
            d.workforce_produced == 0
                && d.workforce_required > 0
                && d.workforce_required < disrupted_planet
                && d.power_required <= sys_a_power
                && d.fuel_type == 0
        })
        .cloned()
        .expect("SYS_A zero-fuel upgrade fitting under disrupted planet workforce");
    let max_exp_full = planet_wf - hub_a_local.workforce_required;
    let max_exp_disrupted = disrupted_planet - hub_a_local.workforce_required;
    assert!(max_exp_disrupted < max_exp_full);

    let hub_b_upgrades: Vec<UpgradeDefinition> = if let Some(u) = defs.iter().find(|d| {
        d.workforce_produced == 0
            && d.workforce_required > max_exp_disrupted
            && d.workforce_required <= max_exp_full
            && d.power_required <= sys_b_power
    }) {
        vec![u.clone()]
    } else {
        let mut pair = Vec::new();
        'outer: for (i, a) in defs.iter().enumerate() {
            if a.workforce_produced > 0 || a.workforce_required == 0 || a.power_required > sys_b_power {
                continue;
            }
            for b in defs.iter().skip(i + 1) {
                if b.workforce_produced > 0
                    || b.workforce_required == 0
                    || (!a.exclusive_group.is_empty() && a.exclusive_group == b.exclusive_group)
                {
                    continue;
                }
                let tot_wf = a.workforce_required + b.workforce_required;
                if a.power_required + b.power_required <= sys_b_power
                    && tot_wf > max_exp_disrupted
                    && tot_wf <= max_exp_full
                {
                    pair = vec![a.clone(), b.clone()];
                    break 'outer;
                }
            }
        }
        assert!(!pair.is_empty(), "expected SYS_B upgrades straddling [{max_exp_disrupted}, {max_exp_full}]");
        pair
    };

    let sys_h = ctx.systems.get_or_spawn(SYS_A);
    for (bid, tid) in [
        (HUB_A, TYPE_SOV_HUB),
        (skyhook_a, TYPE_SKYHOOK),
        (skyhook_b, TYPE_SKYHOOK),
        (den_a, TYPE_MERC_DEN),
        (den_b, TYPE_MERC_DEN),
        (mto_ball_a, 12_345),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: DVec3::new(150_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0e8,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, uname) in [(PILOT_ID, "sov_dir"), (dir_b_id, "sov_dir_b"), (den_owner, "raider")] {
            snap.accounts.insert(
                uname.into(),
                AccountRecord {
                    user_id: cid as u64,
                    username: uname.into(),
                    password_token: "hash".into(),
                    role: service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            snap.sessions.insert(
                cid as u64,
                OnlineSessionRecord {
                    user_id: cid as u64,
                    username: uname.into(),
                    session_id: cid as u64,
                    character_id: Some(cid),
                    connected_at_unix: now,
                    computer_hash: None,
                },
            );
        }
        snap.characters.insert(
            PILOT_ID,
            CharacterRecord {
                character_id: PILOT_ID,
                user_id: PILOT_ID as u64,
                name: "EquinoxDirector".into(),
                corporation_id: CORP_ID,
                alliance_id: Some(alliance_id),
                solar_system_id: SYS_A,
                constellation_id: 20_000_097,
                region_id: 10_000_007,
                active_ship_id: PILOT_SHIP,
                balance: IskCents(10_000_000_00),
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                neural: NeuralAttributes::default(),
                ..Default::default()
            },
        );
        snap.characters.insert(
            dir_b_id,
            CharacterRecord {
                character_id: dir_b_id,
                user_id: dir_b_id as u64,
                name: "EquinoxDirectorB".into(),
                corporation_id: CORP_ID,
                alliance_id: Some(alliance_id),
                solar_system_id: SYS_B,
                ..Default::default()
            },
        );
        snap.characters.insert(
            den_owner,
            CharacterRecord {
                character_id: den_owner,
                user_id: den_owner as u64,
                name: "DenOwner".into(),
                corporation_id: OTHER_CORP,
                solar_system_id: SYS_A,
                ..Default::default()
            },
        );
        let mut corp_a = CorporationRecord::new(CORP_ID, "Equinox Corp", "EQX", PILOT_ID);
        corp_a.alliance_id = Some(alliance_id);
        corp_a.member_roles.insert(PILOT_ID, corp_roles::ROLE_DIRECTOR);
        corp_a.member_roles.insert(dir_b_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(CORP_ID, corp_a);
        snap.corporations.insert(
            OTHER_CORP,
            CorporationRecord::new(OTHER_CORP, "Raider Corp", "RDR", den_owner),
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Equinox Alliance".into(),
                short_name: "EQXA".into(),
                executor_corp_id: CORP_ID,
                member_corp_ids: [CORP_ID].into_iter().collect(),
            },
        );

        for (sid, hid, name) in [(SYS_A, HUB_A, "Hub A"), (SYS_B, HUB_B, "Hub B")] {
            snap.system_states.entry(sid).or_default().sov_alliance_id = Some(alliance_id);
            snap.structures.insert(hid, structure(hid, CORP_ID, sid, TYPE_SOV_HUB));
            if let Some(st) = snap.structures.get_mut(&hid) {
                st.name = name.into();
            }
        }
        snap.sov_runtime.sov_claimed_since_unix.insert(SYS_A, now.saturating_sub(1_000));

        // Seed an older Destroyed Sov Hub tombstone alongside live HUB_A.
        let mut dead_hub = structure(old_hub_a, CORP_ID, SYS_A, TYPE_SOV_HUB);
        dead_hub.state = STATE_DESTROYED.into();
        dead_hub.shield_hp = 0.0;
        dead_hub.armor_hp = 0.0;
        dead_hub.hull_hp = 0.0;
        snap.structures.insert(old_hub_a, dead_hub);

        for (sid, pid, tax) in [(skyhook_a, planet_wf_id, 20), (skyhook_b, planet_2, 10)] {
            let mut sh = structure(sid, CORP_ID, SYS_A, TYPE_SKYHOOK);
            sh.shield_hp = SKYHOOK_BASE_SHIELD_HP;
            sh.armor_hp = SKYHOOK_BASE_ARMOR_HP;
            sh.hull_hp = SKYHOOK_BASE_HULL_HP;
            snap.structures.insert(sid, sh);
            snap.sov_runtime.skyhook_planets.insert(sid, pid);
            snap.sov_runtime.skyhook_active.insert(sid, true);
            snap.sov_runtime.mercenary_den_tax_pct.insert(sid, tax);
        }

        snap.sov_runtime.sov_hubs.insert(
            HUB_A,
            PersistedSovHubData {
                hub_id: HUB_A,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: hub_a_local.type_id,
                    online: true,
                    low_power: false,
                }],
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(SYS_B),
                    amount: max_exp_full,
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );
        let mut fuel_b = std::collections::BTreeMap::new();
        for u in &hub_b_upgrades {
            if u.fuel_type > 0 {
                fuel_b.insert(u.fuel_type, u.fuel_startup + u.fuel_per_hour * 100);
            }
        }
        snap.sov_runtime.sov_hubs.insert(
            HUB_B,
            PersistedSovHubData {
                hub_id: HUB_B,
                upgrades: hub_b_upgrades
                    .iter()
                    .map(|u| PersistedHubUpgrade {
                        type_id: u.type_id,
                        online: true,
                        low_power: false,
                    })
                    .collect(),
                fuel: fuel_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![SYS_A],
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );

        for (did, sid, pid) in [(den_a, skyhook_a, planet_wf_id), (den_b, skyhook_b, planet_2)] {
            snap.structures.insert(did, structure(did, OTHER_CORP, SYS_A, TYPE_MERC_DEN));
            snap.sov_runtime.mercenary_dens.insert(
                did,
                PersistedMercenaryDen {
                    den_id: did,
                    type_id: TYPE_MERC_DEN,
                    owner_char_id: den_owner,
                    skyhook_id: sid,
                    solar_system_id: SYS_A,
                    planet_id: pid,
                    enabled: true,
                    cargo_extraction_enabled: true,
                    development_level: 10,
                    anarchy_level: 10,
                    infomorphs: 20,
                    simulated_at_unix: now,
                    next_activity_generation_unix: now + 3_600,
                },
            );
        }
        let act_key = "d4".repeat(16);
        snap.sov_runtime.mto_site_balls.insert(mto_ball_a, act_key.clone());
        snap.sov_runtime.mercenary_activities.insert(
            act_key,
            PersistedMercenaryActivity {
                uuid: vec![0xD4; 16],
                den_id: den_a,
                solar_system_id: SYS_A,
                dungeon_id: 12_367,
                started: true,
                started_by: den_owner,
                expiry_unix: now + 3_600,
                development_impact: 5,
                anarchy_impact: -5,
                infomorph_bonus: 0,
            },
        );
    });

    let mut dir = sess(PILOT_ID as u64, PILOT_ID, CORP_ID, SYS_A, corp_roles::ROLE_DIRECTOR);
    let mut rx_dir = subscribe(&ctx, &mut dir).await;
    let mut dir_b = sess(dir_b_id as u64, dir_b_id, CORP_ID, SYS_B, corp_roles::ROLE_DIRECTOR);
    let mut rx_b = subscribe(&ctx, &mut dir_b).await;

    // 1. Combat destruction of SKYHOOK_A cleans tax, attached DEN_A, MTO/balls, emits notifications,
    //    clamps HUB_A export -> 0, drops HUB_B to low_power, and leaves a Destroyed Skyhook tombstone.
    let out_sky = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = destroy_structure(&ctx, snap, &mut ext, skyhook_a, den_owner, now);
        save_ext_state(snap, &ext);
        out
    });
    flush_disposal_quasar_notices(&ctx, &out_sky, now);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_a].state, STATE_DESTROYED);
        assert_eq!(snap.structures[&den_a].state, STATE_DESTROYED);
        assert!(!snap.sov_runtime.mercenary_den_tax_pct.contains_key(&skyhook_a));
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_a));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&"d4".repeat(16)));
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_a));
        assert_eq!(exported_amount(snap, HUB_A), 0);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_b));
    });
    let (has_skyhook_a, has_den_a, has_mto_a) = sys_h
        .test_with_ballpark_async(move |bp| {
            (
                bp.balls.contains_key(&skyhook_a),
                bp.balls.contains_key(&den_a),
                bp.balls.contains_key(&mto_ball_a),
            )
        })
        .await;
    assert!(!has_skyhook_a);
    assert!(!has_den_a);
    assert!(!has_mto_a);
    assert!(ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&den_owner)
        .into_iter()
        .flatten()
        .any(|n| n.event == "OnMyMercenaryDenRemoved"));
    let dir_step1 = drain_notices(&mut rx_dir);
    assert!(dir_step1.iter().any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(dir_step1.iter().any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    let b_step1 = drain_notices(&mut rx_b);
    assert!(b_step1.iter().any(|u| u.ends_with("upgrade.api.HubUpgradesSimulatedNotice")));

    // 2. Anchor a replacement SKYHOOK_NEW at the SAME planet_wf_id while Destroyed `skyhook_a`
    //    and `old_hub_a` tombstones remain in snap.structures. Verify GetMyCorporationSkyhooks and
    //    GetCurrentSovData ignore tombstones, AND attach Stage 5 `den_new` in HullVulnerable.
    let vuln_end = now + 200;
    ctx.db.with_mut_snapshot(|snap| {
        let mut sh_new = structure(skyhook_new, CORP_ID, SYS_A, TYPE_SKYHOOK);
        sh_new.shield_hp = SKYHOOK_BASE_SHIELD_HP;
        sh_new.armor_hp = SKYHOOK_BASE_ARMOR_HP;
        sh_new.hull_hp = SKYHOOK_BASE_HULL_HP;
        snap.structures.insert(skyhook_new, sh_new);
        snap.sov_runtime.skyhook_planets.insert(skyhook_new, planet_wf_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_new, true);
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_new, 25);

        let mut dnew = structure(den_new, OTHER_CORP, SYS_A, TYPE_MERC_DEN);
        dnew.state = "HullVulnerable".into();
        dnew.reinforced_until_unix = vuln_end;
        dnew.shield_hp = 0.0;
        snap.structures.insert(den_new, dnew);
        snap.sov_runtime.mercenary_dens.insert(
            den_new,
            PersistedMercenaryDen {
                den_id: den_new,
                type_id: TYPE_MERC_DEN,
                owner_char_id: den_owner,
                skyhook_id: skyhook_new,
                solar_system_id: SYS_A,
                planet_id: planet_wf_id,
                enabled: false,
                cargo_extraction_enabled: false,
                development_level: 10,
                anarchy_level: 100, // Stage 5 -> 30% once vuln_end re-enables!
                infomorphs: 0,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_new, (now + 50, vuln_end, 0));

        // Seed HUB_A export > max_exp_full (`planet_wf`) and restore local upgrade power so
        // tick_sov_gameplay first proves clamp-before-overload keeps `hub_a_local` online!
        if let Some(ha) = snap.sov_runtime.sov_hubs.get_mut(&HUB_A) {
            for u in &mut ha.upgrades {
                u.low_power = false;
            }
            ha.workforce = PersistedWorkforceMode::Export {
                destination: Some(SYS_B),
                amount: planet_wf,
            };
        }
        if let Some(hb) = snap.sov_runtime.sov_hubs.get_mut(&HUB_B) {
            for u in &mut hb.upgrades {
                u.low_power = false;
            }
        }
    });

    // GetMyCorporationSkyhooks returns [skyhook_b, skyhook_new] (never `skyhook_a` tombstone).
    let my_sh = ctx
        .dispatch_rpc(&mut dir, "colonyResourcesMgr", None, "GetMyCorporationSkyhooks", &[], &HashMap::new(), None)
        .await
        .expect("GetMyCorporationSkyhooks");
    let PyRep::List(sh_rows) = my_sh else { panic!("expected list"); };
    let mut returned_ids: Vec<i64> = sh_rows
        .iter()
        .filter_map(|r| match r {
            PyRep::KeyVal(kv) => kv.iter().find_map(|(k, v)| (k == "skyhookID").then(|| v.as_i64()).flatten()),
            _ => None,
        })
        .collect();
    returned_ids.sort_unstable();
    assert_eq!(returned_ids, vec![skyhook_b, skyhook_new]);

    // GetCurrentSovData returns live HUB_A (never `old_hub_a` tombstone) natively as wire Rowset.
    let sov_data = ctx
        .dispatch_rpc(&mut dir, "map", None, "GetCurrentSovData", &[], &HashMap::new(), None)
        .await
        .expect("GetCurrentSovData");
    let PyRep::Rowset {
        columns: raw_hdr,
        rows: sov_rows,
    } = sov_data else {
        panic!("expected Rowset from GetCurrentSovData");
    };
    let r_idx = |name: &str| raw_hdr.iter().position(|c| c == name).unwrap();
    let sys_a_row = sov_rows
        .iter()
        .find(|r| r.get(r_idx("solarSystemID")) == Some(&PyRep::Integer(i64::from(SYS_A))))
        .expect("SYS_A sov row");
    assert_eq!(sys_a_row.get(r_idx("locationID")), Some(&PyRep::Integer(i64::from(SYS_A))));
    assert_eq!(sys_a_row.get(r_idx("claimStructureID")), Some(&PyRep::Integer(HUB_A)));

    let wire_sov = ctx
        .dispatch_rpc(
            &mut dir,
            "map",
            None,
            "GetCurrentSovData",
            &[PyRep::Integer(i64::from(SYS_A))],
            &HashMap::new(),
            Some("machoNet:26000"),
        )
        .await
        .expect("GetCurrentSovData MachoNet wire");
    let PyRep::Rowset {
        columns: sov_hdr,
        rows: sov_lines,
    } = wire_sov else {
        panic!("expected Rowset from wire GetCurrentSovData");
    };
    assert_eq!(
        sov_hdr,
        vec![
            "locationID",
            "solarSystemID",
            "allianceID",
            "corporationID",
            "claimStructureID",
        ]
    );
    assert_eq!(sov_lines.len(), 1);
    assert_eq!(sov_lines[0][0], PyRep::Integer(i64::from(SYS_A)));
    assert_eq!(sov_lines[0][4], PyRep::Integer(HUB_A));

    // 2a. Pre-vuln_end tick clamps over-export to max_exp_full without overloading HUB_A or HUB_B.
    tick_sov_gameplay(&ctx, now + 10).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(exported_amount(snap, HUB_A), max_exp_full);
        assert!(!snap.sov_runtime.sov_hubs[&HUB_A].upgrades[0].low_power);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, max_exp_full)]);
        assert!(!snap.sov_runtime.sov_hubs[&HUB_B].upgrades.iter().any(|u| u.low_power));
    });

    // 2b. Advancing past vuln_end re-enables Stage 5 `den_new` AND reconciles workforce cascade.
    let _ = drain_notices(&mut rx_dir);
    let _ = drain_notices(&mut rx_b);
    ctx.advance_secs(200);
    tick_sov_gameplay(&ctx, ctx.now_unix()).await;

    ctx.db.with_snapshot(|snap| {
        assert!(snap.sov_runtime.mercenary_dens[&den_new].enabled);
        assert_eq!(snap.structures[&den_new].state, "ShieldVulnerable");
        assert_eq!(
            compute_hub_resources(snap, &ctx.sde, HUB_A).unwrap().workforce_local,
            disrupted_planet
        );
        assert_eq!(exported_amount(snap, HUB_A), max_exp_disrupted);
        assert!(!snap.sov_runtime.sov_hubs[&HUB_A].upgrades[0].low_power);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, max_exp_disrupted)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades.last().unwrap().low_power);
    });
    let dir_notices = drain_notices(&mut rx_dir);
    assert!(dir_notices.iter().any(|u| u.ends_with("mercenaryden.api.EvolutionStateChangedNotice")));
    assert!(dir_notices.iter().any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(dir_notices.iter().any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    let b_notices = drain_notices(&mut rx_b);
    assert!(b_notices.iter().any(|u| u.ends_with("upgrade.api.HubUpgradesSimulatedNotice")));

    // 3. sovMgr.DestroySkyhooks V24 list destroys only selected Skyhook(s) and ignores destroyed tombstones!
    let destroyed_one = ctx
        .dispatch_rpc(
            &mut dir,
            "sovMgr",
            None,
            "DestroySkyhooks",
            &[PyRep::List(vec![PyRep::Integer(skyhook_new)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("DestroySkyhooks selective single skyhook");
    assert_eq!(destroyed_one, PyRep::Integer(1));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&skyhook_new));
        assert!(snap.structures.contains_key(&skyhook_b));
    });

    let destroyed_rest = ctx
        .dispatch_rpc(
            &mut dir,
            "sovMgr",
            None,
            "DestroySkyhooks",
            &[PyRep::List(vec![PyRep::Integer(skyhook_b)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("DestroySkyhooks remaining skyhook ignoring destroyed tombstone");
    assert_eq!(destroyed_rest, PyRep::Integer(1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_a].state, STATE_DESTROYED);
        assert!(!snap.structures.contains_key(&skyhook_new));
        assert!(!snap.structures.contains_key(&skyhook_b));
        assert_eq!(snap.structures[&den_new].state, STATE_DESTROYED);
        assert_eq!(snap.structures[&den_b].state, STATE_DESTROYED);
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_new));
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_b));
        assert!(!snap.sov_runtime.mercenary_den_tax_pct.contains_key(&skyhook_new));
        assert_eq!(exported_amount(snap, HUB_A), 0);
    });

    // 4. Combat destroy_structure on HUB_A while an intact third-party Skyhook + Den exist in SYS_A:
    //    clears sov/hacked/fuel-ACL state, reconciles HUB_B to low_power, and keeps third-party Den intact.
    let skyhook_3: i64 = 1_000_910_199;
    let den_3: i64 = 1_000_920_199;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_3, structure(skyhook_3, CORP_ID, SYS_A, TYPE_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_3, planet_wf_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_3, true);
        snap.structures.insert(den_3, structure(den_3, OTHER_CORP, SYS_A, TYPE_MERC_DEN));
        snap.sov_runtime.mercenary_dens.insert(
            den_3,
            PersistedMercenaryDen {
                den_id: den_3,
                type_id: TYPE_MERC_DEN,
                owner_char_id: den_owner,
                skyhook_id: skyhook_3,
                solar_system_id: SYS_A,
                planet_id: planet_wf_id,
                enabled: true,
                anarchy_level: 10,
                ..Default::default()
            },
        );
        snap.sov_runtime.hacked_hubs.insert(HUB_A, ctx.now_unix() + 600);
        snap.sov_runtime.fuel_access_groups.insert(HUB_A, 99);
        if let Some(ha) = snap.sov_runtime.sov_hubs.get_mut(&HUB_A) {
            ha.workforce = PersistedWorkforceMode::Export {
                destination: Some(SYS_B),
                amount: max_exp_full,
            };
        }
        if let Some(hb) = snap.sov_runtime.sov_hubs.get_mut(&HUB_B) {
            for u in &mut hb.upgrades {
                u.low_power = false;
            }
        }
    });
    let _ = drain_notices(&mut rx_dir);
    let _ = drain_notices(&mut rx_b);
    let now2 = ctx.now_unix();
    let out_hub = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = destroy_structure(&ctx, snap, &mut ext, HUB_A, den_owner, now2);
        save_ext_state(snap, &ext);
        out
    });
    flush_disposal_quasar_notices(&ctx, &out_hub, now2);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&HUB_A].state, STATE_DESTROYED);
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&HUB_A));
        assert!(!snap.sov_runtime.hacked_hubs.contains_key(&HUB_A));
        assert!(!snap.sov_runtime.fuel_access_groups.contains_key(&HUB_A));
        assert_eq!(snap.system_states[&SYS_A].sov_alliance_id, None);
        assert_eq!(imports(snap, &ctx.sde, HUB_B), vec![(SYS_A, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&HUB_B].upgrades[0].low_power);
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_3));
    });
    let b_step4 = drain_notices(&mut rx_b);
    assert!(b_step4.iter().any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(b_step4.iter().any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    assert!(b_step4.iter().any(|u| u.ends_with("upgrade.api.HubUpgradesSimulatedNotice")));
}
