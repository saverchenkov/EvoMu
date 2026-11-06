//! Deep branch-coverage and mutation-killing unit tests for `quasar_char_skill_services`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use crate::services::quasar_char_skill_services::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, SystemDynamicState, UpwellStructureRecord, WorldRepository,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_string_field,
    encode_varint_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry, WireMessage, WireValue,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> (PublicGatewayRouter, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    (router, db, sde)
}

fn make_ctx(uid: Option<u32>, cid: Option<u32>, role: u64) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session(
        uid.map(|u| u as i32),
        cid,
        Some(98_000_001),
        role,
        "tranquility",
        vec![1],
    );
    ctx.service_role_mask = role;
    ctx
}

fn decode_first_varint(bytes: &[u8]) -> Option<u64> {
    let wm = WireMessage::decode(bytes).ok()?;
    wm.fields.into_iter().find_map(|f| match f.value {
        WireValue::Varint(v) => Some(v),
        _ => None,
    })
}

#[test]
fn shipcaster_eligibility_branches_starter_system_locks_cooldown_and_ship_in_space() {
    let (router, db, sde) = new_router();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let ship_id = 100_000_001_i64;
    let hisec_sys = 30_000_142_u32;
    let null_sys = sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let handlers = router.handlers.read().unwrap();
    let sc_general = &handlers["eve.ship.api.CanJumpStarterSpaceShipCasterRequest"];
    let sc_starter = &handlers["eve.starterspace.api.CanJumpShipCasterRequest"];
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Req");

    // 1. Unauthenticated context rejected with STATUS_UNAUTHORIZED
    let unauth = make_ctx(Some(1), None, 0);
    assert_eq!(sc_general(&unauth, &dummy_any, &empty_msg).unwrap_err().0, STATUS_UNAUTHORIZED);

    // 2. Non-admin querying another character rejected with STATUS_FORBIDDEN; admin allowed
    let player_ctx = make_ctx(Some(1), Some(cid as u32), 0);
    let admin_ctx = make_ctx(Some(1), Some(cid as u32), u64::MAX);
    let mut other_msg = DynamicQuasarMessage::new("Req");
    other_msg.set(1, DynamicValue::Uint64(other_cid as u64));
    assert_eq!(sc_general(&player_ctx, &dummy_any, &other_msg).unwrap_err().0, STATUS_FORBIDDEN);
    assert!(sc_general(&admin_ctx, &dummy_any, &other_msg).is_ok());

    // 3. No shipcaster in cluster -> returns 0 (false)
    let res = sc_general(&player_ctx, &dummy_any, &empty_msg).unwrap();
    assert_eq!(decode_first_varint(&res.value), Some(0));

    // 4. Seed starter_launchers in cluster_misc_state and put character's active ship in space in hisec
    db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "cluster_misc_state".into(),
            r#"{"starter_launchers":[1001]}"#.into(),
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = hisec_sys;
            ch.active_ship_id = ship_id;
            ch.jump_cooldown_until_unix = 0;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: i64::from(hisec_sys),
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let res_ok = sc_starter(&player_ctx, &dummy_any, &empty_msg).unwrap();
    assert_eq!(decode_first_varint(&res_ok.value), Some(1));

    // 5. Move character + ship to null-sec: sc_starter requires >= 0.45 sec (returns 0), sc_general returns 1
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().solar_system_id = null_sys;
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(null_sys);
    });
    assert_eq!(
        decode_first_varint(&sc_starter(&player_ctx, &dummy_any, &empty_msg).unwrap().value),
        Some(0)
    );
    assert_eq!(
        decode_first_varint(&sc_general(&player_ctx, &dummy_any, &empty_msg).unwrap().value),
        Some(1)
    );

    // 6. Active Zarzakh gate lock blocks shipcaster jump; expired lock does not
    let now = player_ctx.now_unix();
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(cid, (50_000_001, now + 600));
    });
    assert_eq!(
        decode_first_varint(&sc_general(&player_ctx, &dummy_any, &empty_msg).unwrap().value),
        Some(0)
    );
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(cid, (50_000_001, now.saturating_sub(1)));
    });
    assert_eq!(
        decode_first_varint(&sc_general(&player_ctx, &dummy_any, &empty_msg).unwrap().value),
        Some(1)
    );

    // 7. Jump cooldown active -> returns 0
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().jump_cooldown_until_unix = now + 300;
    });
    assert_eq!(
        decode_first_varint(&sc_general(&player_ctx, &dummy_any, &empty_msg).unwrap().value),
        Some(0)
    );

    // 8. Fallback structure scan when cluster_misc_state blob is removed
    let mut sde_mut = (*sde).clone();
    let mut sc_type = sde.types.values().next().unwrap().clone();
    sc_type.type_id = 81000;
    sc_type.group_id = 1404;
    sc_type.category_id = 65;
    sc_type.name = "Interstellar Shipcaster".into();
    sde_mut.types.insert(81000, sc_type);
    let router2 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::new(sde_mut),
    );
    db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove("cluster_misc_state");
        snap.characters.get_mut(&cid).unwrap().jump_cooldown_until_unix = 0;
        snap.structures.insert(
            1_000_000_810,
            UpwellStructureRecord {
                structure_id: 1_000_000_810,
                type_id: 81000,
                owner_corp_id: 98_000_001,
                solar_system_id: hisec_sys,
                name: "Shipcaster".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let h2 = router2.handlers.read().unwrap();
    assert_eq!(
        decode_first_varint(
            &h2["eve.ship.api.CanJumpStarterSpaceShipCasterRequest"](
                &player_ctx,
                &dummy_any,
                &empty_msg
            )
            .unwrap()
            .value
        ),
        Some(1)
    );
}

#[test]
fn stargate_locks_structures_sovereignty_and_not_implemented_endpoints() {
    let (router, db, _sde) = new_router();
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let player_ctx = make_ctx(Some(1), Some(cid), 0);
    let admin_ctx = make_ctx(Some(1), Some(cid), u64::MAX);
    let zarzakh = 30_100_000_u32;

    // 1. DATA GAP endpoints return STATUS_NOT_IMPLEMENTED
    for req_name in [
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        "eve_public.ship.unlock.ClaimRewardRequest",
        "eve.star.resources.api.GetPowerProductionRequest",
        "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
        "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
    ] {
        let empty = DynamicQuasarMessage::new(req_name);
        assert_eq!(
            handlers[req_name](&player_ctx, &dummy_any, &empty).unwrap_err().0,
            STATUS_NOT_IMPLEMENTED
        );
    }

    // 2. Zarzakh stargate lock GetRequest, ValidateJumpRequest, DeleteCommanded
    let get_lock = &handlers["eve.stargate.lock.api.GetRequest"];
    let val_jump = &handlers["eve.stargate.lock.api.ValidateJumpRequest"];
    let del_lock = &handlers["eve.stargate.lock.api.DeleteCommanded"];

    let mut mismatch_msg = DynamicQuasarMessage::new("Lock");
    mismatch_msg.set(1, DynamicValue::Uint64(90_000_099));
    mismatch_msg.set(2, DynamicValue::Uint32(zarzakh));
    mismatch_msg.set(3, DynamicValue::Uint64(50_000_001));
    assert_eq!(get_lock(&player_ctx, &dummy_any, &mismatch_msg).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(val_jump(&player_ctx, &dummy_any, &mismatch_msg).unwrap_err().0, STATUS_FORBIDDEN);

    // Non-Zarzakh system -> GetRequest returns NOT_FOUND, ValidateJump returns 1
    let mut jita_lock_msg = DynamicQuasarMessage::new("Lock");
    jita_lock_msg.set(2, DynamicValue::Uint32(30_000_142));
    jita_lock_msg.set(3, DynamicValue::Uint64(50_000_001));
    assert_eq!(get_lock(&player_ctx, &dummy_any, &jita_lock_msg).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(
        decode_first_varint(&val_jump(&player_ctx, &dummy_any, &jita_lock_msg).unwrap().value),
        Some(1)
    );

    // Seed active Zarzakh lock on gate 50_000_111
    let now = player_ctx.now_unix();
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks
            .insert(i64::from(cid), (50_000_111, now + 3600));
    });
    let mut zz_msg = DynamicQuasarMessage::new("Lock");
    zz_msg.set(2, DynamicValue::Uint32(zarzakh));
    zz_msg.set(3, DynamicValue::Uint64(50_000_111));
    assert!(get_lock(&player_ctx, &dummy_any, &zz_msg).is_ok());
    // Same gate -> allowed (1); different gate -> blocked (0)
    assert_eq!(
        decode_first_varint(&val_jump(&player_ctx, &dummy_any, &zz_msg).unwrap().value),
        Some(1)
    );
    zz_msg.set(3, DynamicValue::Uint64(50_000_222));
    assert_eq!(
        decode_first_varint(&val_jump(&player_ctx, &dummy_any, &zz_msg).unwrap().value),
        Some(0)
    );

    // DeleteCommanded: non-admin forbidden; wrong system not found; valid deletes lock; second delete not found
    let mut del_msg = DynamicQuasarMessage::new("Del");
    del_msg.set(2, DynamicValue::Int64(i64::from(cid)));
    del_msg.set(3, DynamicValue::Uint32(30_000_142));
    assert_eq!(del_lock(&player_ctx, &dummy_any, &del_msg).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(del_lock(&admin_ctx, &dummy_any, &del_msg).unwrap_err().0, STATUS_NOT_FOUND);
    del_msg.set(3, DynamicValue::Uint32(zarzakh));
    assert!(del_lock(&admin_ctx, &dummy_any, &del_msg).is_ok());
    assert_eq!(del_lock(&admin_ctx, &dummy_any, &del_msg).unwrap_err().0, STATUS_NOT_FOUND);

    // 3. eve.structure.api.GetRequest: active vs Destroyed vs missing
    let st_id = 1_000_000_950_i64;
    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let mut st_msg = DynamicQuasarMessage::new("St");
    st_msg.set(1, DynamicValue::Int64(st_id));
    assert!(handlers["eve.structure.api.GetRequest"](&admin_ctx, &dummy_any, &st_msg).is_ok());
    db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(
        handlers["eve.structure.api.GetRequest"](&admin_ctx, &dummy_any, &st_msg).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // 4. Sovereignty GetClaimRequest, GetAllClaimsRequest, and ADM GetLevelRequest
    let sys_id = 30_000_142_u32;
    let hub_id = 1_000_000_960_i64;
    db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                sov_alliance_id: Some(99_000_001),
                military_index: 5,
                industrial_index: 5,
                strategic_index: 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(hub_id, Default::default());
        snap.sov_runtime.sov_claimed_since_unix.insert(sys_id, 1_700_000_000);
    });
    let mut claim_msg = DynamicQuasarMessage::new("Claim");
    claim_msg.set(1, DynamicValue::Uint32(sys_id));
    let claim_resp = handlers["eve.sovereignty.api.GetClaimRequest"](&player_ctx, &dummy_any, &claim_msg).unwrap();
    assert_eq!(decode_first_varint(&claim_resp.value), Some(0));

    let all_claims = handlers["eve.sovereignty.api.GetAllClaimsRequest"](
        &player_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("All"),
    )
    .unwrap();
    assert!(!all_claims.value.is_empty());

    let adm_resp = handlers["eve.sovereignty.activitydefensemodifier.api.GetLevelRequest"](
        &player_ctx,
        &dummy_any,
        &claim_msg,
    )
    .unwrap();
    assert!(decode_first_varint(&adm_resp.value).unwrap_or(0) > 10);

    let mut bad_sys_msg = DynamicQuasarMessage::new("BadSys");
    bad_sys_msg.set(1, DynamicValue::Uint32(99_999_999));
    assert_eq!(
        handlers["eve.sovereignty.activitydefensemodifier.api.GetLevelRequest"](
            &player_ctx,
            &dummy_any,
            &bad_sys_msg
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
}

#[test]
fn agents_acls_monolith_and_wire_dynamic_decoding_branches() {
    let (router, db, sde) = new_router();
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let player_ctx = make_ctx(Some(1), Some(90_000_001), 0);

    // 1. Agent GetRequest and SearchRequest across String, Bytes, and nested Repeated inputs
    let first_agent = sde.agents.values().next().expect("SDE has agents");
    let mut ag_get = DynamicQuasarMessage::new("AgentGet");
    ag_get.set(1, DynamicValue::Int64(first_agent.agent_id));
    assert!(handlers["eve.agent.GetRequest"](&player_ctx, &dummy_any, &ag_get).is_ok());

    ag_get.set(1, DynamicValue::Int64(999_999_999));
    assert_eq!(
        handlers["eve.agent.GetRequest"](&player_ctx, &dummy_any, &ag_get).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    let search_tok = first_agent
        .name
        .split_whitespace()
        .next()
        .unwrap_or("a")
        .to_string();
    let mut ag_search_str = DynamicQuasarMessage::new("Search");
    ag_search_str.set(1, DynamicValue::String(search_tok.clone()));
    let sr1 = handlers["eve.agent.SearchRequest"](&player_ctx, &dummy_any, &ag_search_str).unwrap();
    assert!(!sr1.value.is_empty());

    let mut sub_rep = DynamicQuasarMessage::new("Sub");
    sub_rep.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::String(search_tok.clone()),
            DynamicValue::Bytes(b"".to_vec()),
        ]),
    );
    let mut ag_search_nested = DynamicQuasarMessage::new("Search");
    ag_search_nested.set(1, DynamicValue::Message(Box::new(sub_rep)));
    let sr2 = handlers["eve.agent.SearchRequest"](&player_ctx, &dummy_any, &ag_search_nested).unwrap();
    assert!(!sr2.value.is_empty());

    // 2. Custom ACL registry with alliances, corporations, characters, and everyone=true
    let custom_acls = vec![QuasarAclRecord {
        acl_id: 77,
        name: "Alliance Coalition ACL".into(),
        description: "Full membership test".into(),
        version: 5,
        characters: vec![(90_000_001, 3), (90_000_002, 1)],
        corporations: vec![(98_000_001, 2)],
        alliances: vec![(99_000_001, 1)],
        everyone: true,
    }];
    db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "quasar_acl_registry".into(),
            serde_json::to_string(&custom_acls).unwrap(),
        );
    });

    let mut acl_req = DynamicQuasarMessage::new("Acl");
    acl_req.set(1, DynamicValue::Uint32(77));
    acl_req.set(2, DynamicValue::Uint64(90_000_001));
    assert!(handlers["eve.acl.api.GetRequest"](&player_ctx, &dummy_any, &acl_req).is_ok());
    let cm_mgr = handlers["eve.acl.api.CanManageRequest"](&player_ctx, &dummy_any, &acl_req).unwrap();
    assert_eq!(decode_first_varint(&cm_mgr.value), Some(1));

    acl_req.set(2, DynamicValue::Uint64(90_000_002));
    let cm_non_mgr = handlers["eve.acl.api.CanManageRequest"](&player_ctx, &dummy_any, &acl_req).unwrap();
    assert_eq!(decode_first_varint(&cm_non_mgr.value), Some(0));

    acl_req.set(1, DynamicValue::Uint32(9999));
    assert_eq!(
        handlers["eve.acl.api.GetRequest"](&player_ctx, &dummy_any, &acl_req).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve.acl.api.CanManageRequest"](&player_ctx, &dummy_any, &acl_req).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // 3. Monolith legacy json with echo string & character location for unknown character
    let mut json_req = DynamicQuasarMessage::new("Json");
    json_req.set(3, DynamicValue::String("hello_monolith".into()));
    let json_resp = handlers["eve.monolith.legacy.json.Request"](&player_ctx, &dummy_any, &json_req).unwrap();
    assert!(String::from_utf8_lossy(&json_resp.value).contains("hello_monolith"));

    let mut unknown_char_msg = DynamicQuasarMessage::new("Char");
    unknown_char_msg.set(1, DynamicValue::Uint64(999_888_777));
    let miss_cnt = handlers["eve.mission.api.GetCompletedCountRequest"](
        &player_ctx,
        &dummy_any,
        &unknown_char_msg,
    )
    .unwrap();
    assert_eq!(decode_first_varint(&miss_cnt.value), Some(0));
    assert!(handlers["eve.monolith.character.location.GetRequest"](
        &player_ctx,
        &dummy_any,
        &unknown_char_msg
    )
    .is_ok());

    // 4. decode_wire_dynamic with Fixed64, Fixed32, nested submessage, and raw non-message bytes
    let mut sub_wire = Vec::new();
    encode_varint_field(1, 42, &mut sub_wire);
    encode_fixed64_field(2, 123_456, &mut sub_wire);
    encode_fixed32_field(3, 789, &mut sub_wire);
    encode_string_field(4, "inner", &mut sub_wire);

    let mut outer_wire = Vec::new();
    encode_varint_field(1, 7, &mut outer_wire);
    encode_fixed64_field(2, 999, &mut outer_wire);
    encode_fixed32_field(3, 111, &mut outer_wire);
    encode_bytes_field(4, &sub_wire, &mut outer_wire);
    encode_bytes_field(5, &[0xFF, 0xFF, 0xFF], &mut outer_wire);

    let dyn_decoded = decode_wire_dynamic("test.FullWire", &outer_wire);
    assert_eq!(dyn_decoded.get(1), Some(&DynamicValue::Uint64(7)));
    assert_eq!(dyn_decoded.get(2), Some(&DynamicValue::Fixed64(999)));
    assert_eq!(dyn_decoded.get(3), Some(&DynamicValue::Fixed32(111)));
    assert!(matches!(dyn_decoded.get(4), Some(DynamicValue::Message(_))));
    assert!(matches!(dyn_decoded.get(5), Some(DynamicValue::Bytes(_))));

    let _ = NoticeScope::Broadcast;
    let _ = STATUS_BAD_REQUEST;
    let _ = HashMap::<u32, u32>::new();
}
