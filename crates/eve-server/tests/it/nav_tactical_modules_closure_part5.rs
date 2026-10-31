//! Live RPC integration tests for:
//! - `rpc:entity.SetSessionCheck`
//! - `rpc:pvpFilamentMgr.values`
//! - `rpc:posMgr.RunMoonProcessCycleforTower`
//! - `feat:pve.events-sites`
//! - `feat:quasar.space`
//! - `feat:quasar.bubble`
//! - `feat:quasar.ship`
//! - `feat:quasar.capacitor`
//! - `feat:quasar.module`
//! - `feat:quasar.probe`
//! - `feat:quasar.wormhole`
//! - `feat:quasar.effect`

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{drain, subscribe};
use eve_db::{CharacterRecord, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::services::combat_npc_encounters::{
    award_seasonal_event_site_kill, load_seasonal_event_points, load_seasonal_event_sites,
    spawn_seasonal_event_site,
};
use eve_server::services::quasar_tactical_space::{
    encode_capacitor_value, encode_module_models, encode_wormhole_model,
    grant_pending_ship_unlock, publish_combat_effect_notice, publish_probe_scan_result,
};
use std::collections::HashMap;
use std::sync::Arc;

const JITA: u32 = 30_000_142;
const PILOT_A: i64 = 95_100_001;
const PILOT_B: i64 = 95_100_002;
const CORP_A: i64 = 98_100_001;
const CORP_B: i64 = 98_100_002;

fn build_ctx() -> Arc<ServerContext> {
    Arc::new(ServerContext::new())
}

fn seed_pilot(ctx: &ServerContext, cid: i64, corp_id: i64, ship_id: i64, gm: bool) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            cid,
            CharacterRecord {
                activity_status: None,
                character_id: cid,
                user_id: 1,
                name: format!("Pilot {cid}"),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: JITA,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: eve_db::IskCents(1_000_000_00),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: eve_db::NeuralAttributes::default(),
                skill_points: 1_000_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: cid,
                location_id: JITA as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let mut sess = SessionState::new(cid as u64);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(JITA);
    sess.ship_id = Some(ship_id);
    sess.corp_role = 0x01;
    if gm {
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    }
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal with response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload: Vec<u8>,
    corr: &[u8],
) -> EvePublicResponse {
    let kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-nav-tac-05".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_entity_session_check_pvp_filament_values_pos_moon_and_event_sites() {
    // parity: rpc:entity.SetSessionCheck, rpc:pvpFilamentMgr.values, rpc:posMgr.RunMoonProcessCycleforTower, feat:pve.events-sites
    eve_testkit::attest(&[
        "rpc:entity.SetSessionCheck",
        "rpc:pvpFilamentMgr.values",
        "rpc:posMgr.RunMoonProcessCycleforTower",
        "feat:pve.events-sites",
    ]);
    let ctx = build_ctx();
    let mut sess_a = seed_pilot(&ctx, PILOT_A, CORP_A, 100_001_001, true);
    let mut sess_b = seed_pilot(&ctx, PILOT_B, CORP_B, 100_001_002, false);
    let mut unauth = SessionState::new(999);
    let kw = HashMap::new();

    // 1. rpc:entity.SetSessionCheck (positive + negative unauth)
    let ok_sc = ctx
        .dispatch_rpc(&mut sess_a, "entity", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect("entity.SetSessionCheck");
    match ok_sc {
        PyRep::KeyVal(entries) => {
            assert_eq!(entries.get("active"), Some(&PyRep::Bool(false)));
            assert_eq!(entries.get("remainingSeconds"), Some(&PyRep::Integer(0)));
        }
        other => panic!("Expected KeyVal from entity.SetSessionCheck, got {other:?}"),
    }
    let err_sc = ctx
        .dispatch_rpc(&mut unauth, "entity", None, "SetSessionCheck", &[], &kw, None)
        .await;
    assert!(err_sc.is_err(), "unauthenticated entity.SetSessionCheck must fail");

    // 2. rpc:pvpFilamentMgr.values (positive + negative unauth)
    let vals = ctx
        .dispatch_rpc(&mut sess_a, "pvpFilamentMgr", None, "values", &[], &kw, None)
        .await
        .expect("pvpFilamentMgr.values");
    match vals {
        PyRep::List(items) => assert!(!items.is_empty(), "pvpFilamentMgr.values must return events"),
        other => panic!("Expected PyRep::List from pvpFilamentMgr.values, got {other:?}"),
    }
    let err_vals = ctx
        .dispatch_rpc(&mut unauth, "pvpFilamentMgr", None, "values", &[], &kw, None)
        .await;
    assert!(err_vals.is_err(), "unauthenticated pvpFilamentMgr.values must fail");

    // 3. rpc:posMgr.RunMoonProcessCycleforTower (state mutation, persistence, negative/auth)
    let tower_id = 100_500_001;
    let silo_id = 100_500_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            tower_id,
            UpwellStructureRecord {
                structure_id: tower_id,
                type_id: 12235, // Amarr Control Tower
                owner_corp_id: CORP_A,
                solar_system_id: JITA,
                name: "Moon Tower".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            silo_id,
            InventoryItemRecord {
                item_id: silo_id,
                type_id: 14343, // Silo
                owner_id: CORP_A,
                location_id: tower_id,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    // Link silo and provision Atmospheric Gases (16634)
    ctx.dispatch_rpc(
        &mut sess_a,
        "posMgr",
        None,
        "LinkResourceForTower",
        &[PyRep::Integer(tower_id), PyRep::List(vec![PyRep::Integer(silo_id)])],
        &kw,
        None,
    )
    .await
    .expect("LinkResourceForTower");
    ctx.dispatch_rpc(
        &mut sess_a,
        "posMgr",
        None,
        "ChangeStructureProvisionType",
        &[PyRep::Integer(tower_id), PyRep::Integer(silo_id), PyRep::Integer(16634)],
        &kw,
        None,
    )
    .await
    .expect("ChangeStructureProvisionType");

    // Wrong corp must fail
    let wrong_corp = ctx
        .dispatch_rpc(
            &mut sess_b,
            "posMgr",
            None,
            "RunMoonProcessCycleforTower",
            &[PyRep::Integer(tower_id)],
            &kw,
            None,
        )
        .await;
    assert!(wrong_corp.is_err(), "wrong corp must be rejected");

    // Authorized corp runs moon process cycle -> +100 units of 16634 in silo
    let moon_res = ctx
        .dispatch_rpc(
            &mut sess_a,
            "posMgr",
            None,
            "RunMoonProcessCycleforTower",
            &[PyRep::Integer(tower_id)],
            &kw,
            None,
        )
        .await
        .expect("RunMoonProcessCycleforTower");
    assert_eq!(moon_res, PyRep::None);
    let qty = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.location_id == silo_id && it.type_id == 16634)
            .map(|it| it.quantity)
            .unwrap_or(0)
    });
    assert_eq!(qty, 100, "moon process cycle must deposit 100 units of provisioned moon material");

    // 4. feat:pve.events-sites (negative when no event window active, positive when active + NPC kill points)
    let no_event = spawn_seasonal_event_site(&ctx, JITA, "Crimson Site", &[23791], 75, 28668);
    assert!(no_event.is_err(), "spawn_seasonal_event_site must fail when no seasonal event window is active");

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("pve:active_seasonal_campaign_id".into(), "901".into());
    });
    let spawned = spawn_seasonal_event_site(&ctx, JITA, "Crimson Site", &[23791], 75, 28668)
        .expect("spawn_seasonal_event_site");
    let npc_id = *spawned.npc_item_ids.first().expect("npc_item_ids");
    let active_sites = ctx.db.with_snapshot(load_seasonal_event_sites);
    assert_eq!(active_sites.len(), 1);
    let awarded = award_seasonal_event_site_kill(&ctx, PILOT_A, npc_id).expect("award event kill");
    assert_eq!(awarded.0, 75, "event kill must award 75 seasonal event points");
    let persisted_pts = ctx.db.with_snapshot(|snap| load_seasonal_event_points(snap, PILOT_A));
    assert_eq!(persisted_pts, 75);
}

#[tokio::test]
async fn test_quasar_space_bubble_and_ship_unlock_live_rpc() {
    // parity: feat:quasar.space, feat:quasar.bubble, feat:quasar.ship
    eve_testkit::attest(&["feat:quasar.space", "feat:quasar.bubble", "feat:quasar.ship"]);
    let ctx = build_ctx();
    let mut sess_a = seed_pilot(&ctx, PILOT_A, CORP_A, 100_002_001, true);
    let mut unauth = SessionState::new(998);
    let mut rx_a = subscribe(&ctx, &mut sess_a).await;

    // 1. feat:quasar.ship — grant pending unlock, query pending, claim reward, verify notice & negative cases
    grant_pending_ship_unlock(&ctx.db, PILOT_A, 587);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Vec::new(),
        b"corr-ship-unauth",
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    let pending_resp = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Vec::new(),
        b"corr-ship-pend",
    )
    .await;
    assert_eq!(pending_resp.status_code, STATUS_OK);
    let pending_bytes = pending_resp.payload.unwrap().value;
    assert!(!pending_bytes.is_empty(), "pending unlock must contain ship type 587");

    let mut claim_req = Vec::new();
    let mut id_buf = Vec::new();
    encode_varint_field(1, 587, &mut id_buf);
    encode_bytes_field(1, &id_buf, &mut claim_req);
    let claim_resp = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve_public.ship.unlock.ClaimRewardRequest",
        claim_req.clone(),
        b"corr-ship-claim",
    )
    .await;
    assert_eq!(claim_resp.status_code, STATUS_OK);
    let notices = drain(&mut rx_a);
    assert!(
        notices
            .iter()
            .any(|(type_url, _)| type_url.ends_with("eve_public.ship.unlock.ShipsUnlockedNotice")),
        "ClaimRewardRequest must emit ShipsUnlockedNotice"
    );

    // Claiming again must return 404 Not Found
    let claim_again = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve_public.ship.unlock.ClaimRewardRequest",
        claim_req,
        b"corr-ship-claim2",
    )
    .await;
    assert_eq!(claim_again.status_code, STATUS_NOT_FOUND);

    // eve.ship.api.GetRequest
    let mut ship_get_req = Vec::new();
    let mut sid_buf = Vec::new();
    encode_varint_field(1, 100_002_001, &mut sid_buf);
    encode_bytes_field(1, &sid_buf, &mut ship_get_req);
    let ship_get = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve.ship.api.GetRequest",
        ship_get_req,
        b"corr-ship-get",
    )
    .await;
    assert_eq!(ship_get.status_code, STATUS_OK);

    // 2. feat:quasar.bubble — eve.bubble.presence.api.GetInBubbleRequest
    let mut bubble_req = Vec::new();
    let mut b_buf = Vec::new();
    encode_string_field(1, "bubble-jita-01", &mut b_buf);
    encode_bytes_field(1, &b_buf, &mut bubble_req);
    let bubble_resp = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve.bubble.presence.api.GetInBubbleRequest",
        bubble_req,
        b"corr-bubble-1",
    )
    .await;
    assert_eq!(bubble_resp.status_code, STATUS_OK);

    // 3. feat:quasar.space — eve.space.npcstation.api.GetRequest (Jita 4-4 = 60_003_760)
    let mut space_req = Vec::new();
    let mut st_buf = Vec::new();
    encode_varint_field(1, 60_003_760, &mut st_buf);
    encode_bytes_field(1, &st_buf, &mut space_req);
    let space_resp = quasar_rpc(
        &ctx,
        &mut sess_a,
        "eve.space.npcstation.api.GetRequest",
        space_req,
        b"corr-space-1",
    )
    .await;
    assert_eq!(space_resp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_capacitor_module_probe_wormhole_and_effect_models() {
    // parity: feat:quasar.capacitor, feat:quasar.module, feat:quasar.probe, feat:quasar.wormhole, feat:quasar.effect
    eve_testkit::attest(&[
        "feat:quasar.capacitor",
        "feat:quasar.module",
        "feat:quasar.probe",
        "feat:quasar.wormhole",
        "feat:quasar.effect",
    ]);
    let ctx = build_ctx();
    let mut sess_a = seed_pilot(&ctx, PILOT_A, CORP_A, 100_003_001, true);
    let mut rx_a = subscribe(&ctx, &mut sess_a).await;
    let reg = ctx.quasar_gateway.descriptors.clone();

    // 1. feat:quasar.capacitor
    let cap_any = encode_capacitor_value(&reg, 420, 500_000_000).expect("encode_capacitor_value");
    let cap_dyn = reg
        .decode_dynamic("eve.capacitor.Value", &cap_any.value)
        .expect("decode capacitor");
    assert_eq!(cap_dyn.get(1), Some(&DynamicValue::Uint32(420)));
    assert_eq!(cap_dyn.get(2), Some(&DynamicValue::Uint32(500_000_000)));

    // 2. feat:quasar.module
    let (mod_any, type_any) =
        encode_module_models(&reg, 100_003_010, 2048, "Damage Control II").expect("encode_module_models");
    assert!(!mod_any.value.is_empty());
    assert!(!type_any.value.is_empty());

    // 3. feat:quasar.wormhole
    let wh_any = encode_wormhole_model(&reg, 100_003_020, 30764, "K162").expect("encode_wormhole_model");
    assert!(!wh_any.value.is_empty());

    // 4. feat:quasar.probe
    let probe_any = publish_probe_scan_result(
        &ctx.quasar_gateway,
        PILOT_A as u32,
        1,
        10_000,
        ctx.now_unix(),
    )
    .expect("publish_probe_scan_result");
    assert!(!probe_any.value.is_empty());

    // 5. feat:quasar.effect
    let eff_any = publish_combat_effect_notice(
        &ctx.quasar_gateway,
        JITA,
        PILOT_A as u64,
        100_003_010,
        ctx.now_unix(),
    )
    .expect("publish_combat_effect_notice");
    assert!(!eff_any.value.is_empty());

    let notices = drain(&mut rx_a);
    assert!(notices.len() >= 2, "probe and effect notices must be delivered to subscriber");
}
