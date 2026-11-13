//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_sov_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use crate::services::operations_manager::{load_store, save_store};
use eve_db::{
    FwSystemOccupationRecord, PersistedInsurgencyCampaign, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, WireField,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(QuasarDescriptorRegistry::open_default_or_empty))
}

fn setup() -> (Arc<WorldRepository>, PublicGatewayRouter) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let desc = shared_descriptors();
    let router = PublicGatewayRouter::new(desc, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), sde);
    (db, router)
}

fn sess(uid: Option<u64>, cid: Option<i64>, corp: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = corp;
    s.role = role;
    s
}

fn call(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![4; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                value,
            }),
            ..Default::default()
        },
        None,
        false,
        now,
    )
}

fn encode_sint64_varint(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn extract_varints(wire: &WireMessage, field: u32) -> Vec<u64> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_varint())
        .collect()
}

fn extract_messages(wire: &WireMessage, field: u32) -> Vec<WireMessage> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_bytes().and_then(|b| WireMessage::decode(b).ok()))
        .collect()
}

#[test]
fn skyhook_activate_and_deactivate_authorization_and_state_guards() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let owner_corp = 98_400_001_i64;
    let other_corp = 98_400_002_i64;
    let owner_s = sess(Some(1), Some(2110_401), Some(owner_corp), 0);
    let foreign_s = sess(Some(2), Some(2110_402), Some(other_corp), 0);
    let unauth_s = sess(Some(3), None, None, 0);

    let skyhook_id = 1_000_900_001_i64;
    let non_skyhook_id = 1_000_900_002_i64;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = 2110_401;
        ch.user_id = 1;
        ch.name = "SkyhookDir".to_string();
        ch.corporation_id = owner_corp;
        snap.characters.insert(2110_401, ch);
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = owner_corp;
        corp.name = "Skyhook Corp".to_string();
        corp.ticker = "SKY".to_string();
        corp.ceo_id = 2110_401;
        corp.members.insert(2110_401);
        corp.member_roles.insert(2110_401, eve_db::corp_roles::ROLE_DIRECTOR);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(owner_corp, corp);
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: owner_corp,
                solar_system_id: 3000_0142,
                name: "Orbital Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "Anchored".to_string(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            non_skyhook_id,
            UpwellStructureRecord {
                structure_id: non_skyhook_id,
                type_id: 35833, // Fortizar != TYPE_ORBITAL_SKYHOOK
                owner_corp_id: owner_corp,
                solar_system_id: 3000_0142,
                name: "Not Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Intentionally omit `skyhook_planets` entry for `skyhook_id` to verify fallback activate -> 200
    });

    for ep in [
        "eve_public.sovereignty.skyhook.api.ActivateRequest",
        "eve_public.sovereignty.skyhook.api.DeactivateRequest",
    ] {
        // No active character -> 403
        assert_eq!(call(&router, &unauth_s, ep, Vec::new(), now).status_code, STATUS_FORBIDDEN);
        // Missing id -> 400
        assert_eq!(call(&router, &owner_s, ep, Vec::new(), now).status_code, STATUS_BAD_REQUEST);
        // Unknown structure -> 404
        let unk = pb::message(ep).set("skyhook.sequential", 9_999_999u64).must_encode();
        assert_eq!(call(&router, &owner_s, ep, unk, now).status_code, STATUS_NOT_FOUND);
        // Wrong type_id -> 404
        let wrong_type = pb::message(ep)
            .set("skyhook.sequential", non_skyhook_id as u64)
            .must_encode();
        assert_eq!(call(&router, &owner_s, ep, wrong_type, now).status_code, STATUS_NOT_FOUND);
        // Foreign corp -> 403
        let sky_req = pb::message(ep)
            .set("skyhook.sequential", skyhook_id as u64)
            .must_encode();
        assert_eq!(call(&router, &foreign_s, ep, sky_req, now).status_code, STATUS_FORBIDDEN);
    }

    let act_req = pb::message("eve_public.sovereignty.skyhook.api.ActivateRequest")
        .set("skyhook.sequential", skyhook_id as u64)
        .must_encode();
    let deact_req = pb::message("eve_public.sovereignty.skyhook.api.DeactivateRequest")
        .set("skyhook.sequential", skyhook_id as u64)
        .must_encode();
    // Activate when default-active -> 400
    assert_eq!(
        call(&router, &owner_s, "eve_public.sovereignty.skyhook.api.ActivateRequest", act_req.clone(), now)
            .status_code,
        STATUS_BAD_REQUEST
    );
    // Deactivate even without skyhook_planets entry -> 200
    assert_eq!(
        call(&router, &owner_s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", deact_req.clone(), now)
            .status_code,
        STATUS_OK
    );
    // Deactivate when inactive -> 400
    assert_eq!(
        call(&router, &owner_s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", deact_req.clone(), now)
            .status_code,
        STATUS_BAD_REQUEST
    );
    // Activate even without skyhook_planets entry -> 200
    assert_eq!(
        call(&router, &owner_s, "eve_public.sovereignty.skyhook.api.ActivateRequest", act_req, now)
            .status_code,
        STATUS_OK
    );
    // Now deactivate succeeds -> 200
    assert_eq!(
        call(&router, &owner_s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", deact_req, now)
            .status_code,
        STATUS_OK
    );
}

#[test]
fn faction_get_and_search_cover_empire_vs_pirate_fields_and_nested_filters() {
    let (_db, router) = setup();
    let s = sess(Some(1), Some(2110_001), Some(1_000_009), 0);
    let now = 1_760_000_000_u64;

    // 1. Missing/0 faction -> 400
    assert_eq!(
        call(&router, &s, "eve.faction.api.GetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    // 2. Unknown faction -> 404
    let unk = pb::message("eve.faction.api.GetRequest")
        .set("faction.sequential", 599_999u64)
        .must_encode();
    assert_eq!(
        call(&router, &s, "eve.faction.api.GetRequest", unk, now).status_code,
        STATUS_NOT_FOUND
    );

    // 3. Empire faction 500_001 (Caldari State) encodes fields 3 & 4
    let caldari = pb::message("eve.faction.api.GetRequest")
        .set("faction.sequential", 500_001u64)
        .must_encode();
    let r_emp = call(&router, &s, "eve.faction.api.GetRequest", caldari, now);
    assert_eq!(r_emp.status_code, STATUS_OK);
    let w_emp = WireMessage::decode(&r_emp.payload.unwrap().value).unwrap();
    let fac_emp = extract_messages(&w_emp, 1).into_iter().next().unwrap();
    assert!(
        fac_emp.fields.iter().any(|f| f.field_number == 3)
            && fac_emp.fields.iter().any(|f| f.field_number == 4)
    );

    // 4. Non-empire faction 500_010 (Guristas Pirates) omits fields 3 & 4
    let pirate = pb::message("eve.faction.api.GetRequest")
        .set("faction.sequential", 500_010u64)
        .must_encode();
    let r_pir = call(&router, &s, "eve.faction.api.GetRequest", pirate, now);
    assert_eq!(r_pir.status_code, STATUS_OK);
    let w_pir = WireMessage::decode(&r_pir.payload.unwrap().value).unwrap();
    let fac_pir = extract_messages(&w_pir, 1).into_iter().next().unwrap();
    assert!(
        !fac_pir.fields.iter().any(|f| f.field_number == 3)
            && !fac_pir.fields.iter().any(|f| f.field_number == 4)
    );

    // 5. SearchRequest empty/whitespace -> 400
    let ws = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(b"   ".to_vec()),
        }],
    }
    .encode();
    assert_eq!(
        call(&router, &s, "eve.faction.SearchRequest", ws, now).status_code,
        STATUS_BAD_REQUEST
    );

    // 6. SearchRequest nested Message { 1: Repeated([String, String]) } AND filter ("Caldari", "State")
    let nested_filter = pb::message("eve.faction.SearchRequest")
        .add("query.keywords", "Caldari")
        .add("query.keywords", "State")
        .must_encode();
    let r_match = call(&router, &s, "eve.faction.SearchRequest", nested_filter, now);
    assert_eq!(r_match.status_code, STATUS_OK);
    let w_match = WireMessage::decode(&r_match.payload.unwrap().value).unwrap();
    assert!(!extract_messages(&w_match, 1).is_empty());

    // 7. Non-matching search -> empty result bytes / no field 1 entries
    let nomatch = pb::message("eve.faction.SearchRequest")
        .add("query.keywords", "ZzzNonexistentFactionXyz")
        .must_encode();
    let r_none = call(&router, &s, "eve.faction.SearchRequest", nomatch, now);
    assert_eq!(r_none.status_code, STATUS_OK);
    let bytes = r_none.payload.unwrap().value;
    assert!(bytes.is_empty() || extract_messages(&WireMessage::decode(&bytes).unwrap(), 1).is_empty());
}

#[test]
fn fw_system_scores_contribution_set_and_fallbacks_cover_guards_overrides_and_notices() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let player_cid = 2110_501_i64;
    let admin_cid = 2110_502_i64;
    let player_s = sess(Some(51), Some(player_cid), Some(1_000_009), 0);
    let admin_s = sess(Some(52), Some(admin_cid), Some(1_000_009), service_roles::ROLE_ADMIN);
    let unauth_s = sess(Some(53), None, None, 0);

    // Pick a known FW system (e.g. Huola 3000_3067 or seed one in fw_system_occupations) and Jita 3000_0142 (non-FW)
    let fw_sys = 3000_3067_u32;
    let non_fw_sys = 3000_0142_u32;
    db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            fw_sys,
            FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: 500_003,
                occupier_faction_id: 500_003,
                victory_points: 1200,
                victory_points_threshold: 3000,
            },
        );
        snap.fw_runtime.system_victory_points.insert(fw_sys, 2500);
        snap.fw_runtime.system_advantage.insert(fw_sys, (0.25, 0.75));
        let mut ch = snap.characters.get(&90_000_001).unwrap().clone();
        ch.character_id = player_cid;
        ch.solar_system_id = fw_sys;
        snap.characters.insert(player_cid, ch);
    });

    // 1. GetSolarSystemScoresRequest: no sys & no char -> 403
    assert_eq!(
        call(
            &router,
            &unauth_s,
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // 2. Fallback to caller's solar_system_id (fw_sys) and check fw_runtime overrides
    let r_fallback = call(
        &router,
        &player_s,
        "eve_public.faction.activity.GetSolarSystemScoresRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_fallback.status_code, STATUS_OK);

    // 3. Non-FW system -> 400
    let non_fw_req = pb::message("eve.faction.activity.GetSolarSystemScoresRequest")
        .set("solar_system.sequential", u64::from(non_fw_sys))
        .must_encode();
    assert_eq!(
        call(
            &router,
            &player_s,
            "eve.faction.activity.GetSolarSystemScoresRequest",
            non_fw_req,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 4. Increment & Set guards: unauth -> 403, non-admin -> 403, non-FW -> 400
    for ep in [
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
    ] {
        assert_eq!(call(&router, &unauth_s, ep, Vec::new(), now).status_code, STATUS_FORBIDDEN);
        assert_eq!(call(&router, &player_s, ep, Vec::new(), now).status_code, STATUS_FORBIDDEN);
        let bad_sys = pb::message(ep)
            .set("target_system.sequential", u64::from(non_fw_sys))
            .must_encode();
        assert_eq!(call(&router, &admin_s, ep, bad_sys, now).status_code, STATUS_BAD_REQUEST);
    }

    // 5. Increment emits ScoreContributionNotice + SolarSystemScoresNotice
    router.routing_cache.write().unwrap().entries.clear();
    let inc_req = pb::message("eve_public.faction.activity.admin.IncrementContributionScoreRequest")
        .set("target_system.sequential", u64::from(fw_sys))
        .set("adjustment", 100i32)
        .must_encode();
    assert_eq!(
        call(
            &router,
            &admin_s,
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            inc_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    let inc_notices: Vec<String> = router
        .routing_cache
        .read()
        .unwrap()
        .entries
        .iter()
        .filter_map(|e| e.notice.payload.as_ref().map(|a| a.type_url.clone()))
        .collect();
    assert!(inc_notices.iter().any(|u| u.ends_with("ScoreContributionNotice")));
    assert!(inc_notices.iter().any(|u| u.ends_with("SolarSystemScoresNotice")));

    // 6. Set with negative contribution clamps to 0, updates occupier_faction_id (via threshold/faction field), emits ONLY SolarSystemScoresNotice
    router.routing_cache.write().unwrap().entries.clear();
    let target_sys_wire = pb::message("eve_public.solarsystem.Identifier")
        .set("sequential", u64::from(fw_sys))
        .must_encode();
    let target_fac_wire = pb::message("eve_public.faction.Identifier")
        .set("sequential", 500_002u64)
        .must_encode();
    // Negative sint64 for score delta/value & occupier faction in field 3 (intentionally swapped wire fields)
    let set_req = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(target_sys_wire),
            },
            WireField {
                field_number: 2,
                value: WireValue::Varint(encode_sint64_varint(-50)),
            },
            WireField {
                field_number: 3,
                value: WireValue::LengthDelimited(target_fac_wire),
            },
        ],
    }
    .encode();
    let set_resp = call(
        &router,
        &admin_s,
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
        set_req,
        now,
    );
    assert_eq!(set_resp.status_code, STATUS_OK);
    let set_notices: Vec<String> = router
        .routing_cache
        .read()
        .unwrap()
        .entries
        .iter()
        .filter_map(|e| e.notice.payload.as_ref().map(|a| a.type_url.clone()))
        .collect();
    assert!(set_notices.iter().any(|u| u.ends_with("SolarSystemScoresNotice")));
    assert!(!set_notices.iter().any(|u| u.ends_with("ScoreContributionNotice")));

    // 7. systems.GetRequest & stats.character.GetRequest fallback chains
    assert_eq!(
        call(&router, &player_s, "eve.factionwarfare.systems.GetRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player_s, "eve.factionwarfare.stats.character.GetRequest", Vec::new(), now)
            .status_code,
        STATUS_OK
    );
}

#[test]
fn warzone_backfill_pirate_campaign_override_and_operation_recommendations_edges() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let cid = 2110_601_i64;
    let s = sess(Some(61), Some(cid), Some(1_000_009), 0);
    let unauth = sess(Some(62), None, None, 0);

    // Empty fw_system_occupations triggers backfill on warzone.api.GetAllRequest
    db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
    });
    let wz_all = call(&router, &s, "eve.factionwarfare.warzone.api.GetAllRequest", Vec::new(), now);
    assert_eq!(wz_all.status_code, STATUS_OK);
    assert!(db.with_snapshot(|snap| !snap.fw_system_occupations.is_empty()));

    // GetParticipantsRequest: missing id -> 400
    assert_eq!(
        call(
            &router,
            &s,
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // Seed active insurgency campaign overriding pirate faction for warzone 1 (500_001 vs 500_004 -> Guristas 500_010 default; override with Angel 500_011)
    db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            701,
            PersistedInsurgencyCampaign {
                campaign_id: 701,
                pirate_faction_id: 500_011,
                target_faction_id: 500_001,
                fob_solar_system_id: 3000_3067,
                systems: vec![3000_3067],
                created_at_unix: now - 100,
                forecasting_end_unix: now - 50,
                active_end_unix: now + 10_000,
                winddown_end_unix: now + 20_000,
                corruption_points: Default::default(),
                suppression_points: Default::default(),
            },
        );
    });
    for wz_id in [1_u64, 2_u64] {
        let req = pb::message("eve.factionwarfare.warzone.api.GetParticipantsRequest")
            .set("warzone.sequential", wz_id)
            .must_encode();
        let r = call(
            &router,
            &s,
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            req,
            now,
        );
        assert_eq!(r.status_code, STATUS_OK);
        let w = WireMessage::decode(&r.payload.unwrap().value).unwrap();
        if wz_id == 1 {
            let participant_ids: Vec<u64> = extract_messages(&w, 1)
                .into_iter()
                .flat_map(|p| extract_messages(&p, 1).into_iter().chain(extract_messages(&p, 2)))
                .flat_map(|id| extract_varints(&id, 1))
                .collect();
            assert!(
                participant_ids.contains(&500_011),
                "active campaign pirate faction 500_011 should appear in warzone 1 participants, got {participant_ids:?}"
            );
        }
    }

    // Operation recommendations: no active char -> 403
    assert_eq!(
        call(&router, &unauth, "eve_public.operation.recommendation.GetRequest", Vec::new(), now)
            .status_code,
        STATUS_FORBIDDEN
    );

    // Mark first available operation completed for `cid`
    let first_op = shared_sde()
        .operations()
        .ok()
        .and_then(|m| m.keys().filter_map(|k| k.parse::<u64>().ok()).min());
    if let Some(op_id) = first_op {
        db.with_mut_snapshot(|snap| {
            let mut store = load_store(snap);
            store
                .by_character
                .entry(cid)
                .or_default()
                .completed_operations
                .insert(op_id as i64);
            save_store(snap, &store, now);
        });

        // amount = 99 (clamped <= 20), allow_old = false -> excludes completed `op_id`
        let req_new = pb::message("eve_public.operation.recommendation.GetRequest")
            .set("amount", 99u32)
            .set("allow_old", false)
            .must_encode();
        let r_new = call(&router, &s, "eve_public.operation.recommendation.GetRequest", req_new, now);
        assert_eq!(r_new.status_code, STATUS_OK);
        let w_new = WireMessage::decode(&r_new.payload.unwrap().value).unwrap();
        let ops_new: Vec<u64> = extract_messages(&w_new, 1)
            .into_iter()
            .flat_map(|o| extract_varints(&o, 1))
            .collect();
        assert!(ops_new.len() <= 20);
        assert!(!ops_new.contains(&op_id));

        // amount = 0 (clamped >= 1), allow_old = true -> returns >= 1 (if SDE has ops)
        let req_old = pb::message("eve_public.operation.recommendation.GetRequest")
            .set("amount", 0u32)
            .set("allow_old", true)
            .must_encode();
        let r_old = call(&router, &s, "eve_public.operation.recommendation.GetRequest", req_old, now);
        assert_eq!(r_old.status_code, STATUS_OK);
        let w_old = WireMessage::decode(&r_old.payload.unwrap().value).unwrap();
        let ops_old = extract_messages(&w_old, 1);
        assert!(!ops_old.is_empty());
    }
}
