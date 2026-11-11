//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_sov_mercden`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedMercenaryActivity, PersistedMercenaryDen,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository, WorldStateSnapshot,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage, EvePublicRequest,
    EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
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
    let router = PublicGatewayRouter::new(desc, Arc::clone(&db), sde);
    register_handlers(&router, Arc::clone(&db));
    (db, router)
}

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
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
            correlation_uuid: vec![2; 16],
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

fn encode_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field, &sub, out);
}

fn sample_den(den_id: i64, owner_char_id: i64) -> PersistedMercenaryDen {
    PersistedMercenaryDen {
        den_id,
        type_id: 85_230,
        owner_char_id,
        skyhook_id: 1_000_000_555,
        solar_system_id: 3000_0142,
        planet_id: 4000_9001,
        enabled: true,
        cargo_extraction_enabled: true,
        development_level: 10,
        anarchy_level: 20,
        infomorphs: 50,
        simulated_at_unix: 1_700_000_000,
        next_activity_generation_unix: 1_700_010_000,
    }
}

#[test]
fn stage_for_level_covers_all_threshold_boundaries() {
    assert_eq!(stage_for_level(0), 1);
    assert_eq!(stage_for_level(19), 1);
    assert_eq!(stage_for_level(20), 2);
    assert_eq!(stage_for_level(39), 2);
    assert_eq!(stage_for_level(40), 3);
    assert_eq!(stage_for_level(69), 3);
    assert_eq!(stage_for_level(70), 4);
    assert_eq!(stage_for_level(99), 4);
    assert_eq!(stage_for_level(100), 5);
    assert_eq!(stage_for_level(250), 5);
}

#[test]
fn owned_den_checks_not_found_foreign_owner_and_reinforcement_guards() {
    let mut snap = WorldStateSnapshot::default();
    let owner = 2110_001_i64;
    let foreign = 2110_002_i64;
    let den_id = 900_001_i64;

    // Not found -> 404
    assert_eq!(owned_den(&snap, owner, den_id).unwrap_err().0, STATUS_NOT_FOUND);

    // Foreign owner -> 403
    snap.sov_runtime.mercenary_dens.insert(den_id, sample_den(den_id, owner));
    assert_eq!(owned_den(&snap, foreign, den_id).unwrap_err().0, STATUS_FORBIDDEN);

    // Reinforcement + !enabled -> 409
    snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().enabled = false;
    snap.sov_runtime
        .mercenary_den_reinforcement
        .insert(den_id, (1_700_050_000, 1_700_060_000, 80));
    assert_eq!(owned_den(&snap, owner, den_id).unwrap_err().0, STATUS_CONFLICT);

    // Reinforcement + enabled + shield < 50 -> 409
    snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().enabled = true;
    snap.sov_runtime
        .mercenary_den_reinforcement
        .insert(den_id, (1_700_050_000, 1_700_060_000, 49));
    assert_eq!(owned_den(&snap, owner, den_id).unwrap_err().0, STATUS_CONFLICT);

    // Reinforcement + enabled + shield >= 50 -> Ok
    snap.sov_runtime
        .mercenary_den_reinforcement
        .insert(den_id, (1_700_050_000, 1_700_060_000, 50));
    assert_eq!(owned_den(&snap, owner, den_id).unwrap().den_id, den_id);
}

#[test]
fn simulate_den_for_planet_initializes_skips_short_elapsed_and_advances_exact_24h() {
    // 1. simulated_at_unix == 0 initializes timestamp and returns false
    let mut den = sample_den(1, 2110_001);
    den.simulated_at_unix = 0;
    let mut evo_rem = 1234;
    assert!(!simulate_den_for_planet(&mut den, &mut evo_rem, None, 1_700_000_000));
    assert_eq!(den.simulated_at_unix, 1_700_000_000);
    assert_eq!(evo_rem, 1234);

    // 2. now < simulated_at_unix -> returns false
    assert!(!simulate_den_for_planet(&mut den, &mut evo_rem, None, 1_699_999_999));

    // 3. elapsed < 3600 -> returns false
    assert!(!simulate_den_for_planet(&mut den, &mut evo_rem, None, 1_700_003_599));
    assert_eq!(den.development_level, 10);

    // 4. Exact 24h (86_400s): dev +5 (10 -> 15) and anarchy +5 (20 -> 25), caps at 100, preserves evo_rem
    den.development_level = 97;
    den.anarchy_level = 98;
    let mut preserved = 777;
    assert!(simulate_den_for_planet(&mut den, &mut preserved, None, 1_700_000_000 + 86_400));
    assert_eq!(den.development_level, 100);
    assert_eq!(den.anarchy_level, 100);
    assert_eq!(preserved, 777);
}

#[test]
fn complete_mercenary_activity_in_snap_handles_fallback_owner_zero_impacts_cap_and_ball_cleanup() {
    let mut snap = WorldStateSnapshot::default();
    let owner = 2110_001_i64;
    let den_id = 900_002_i64;
    let mut den = sample_den(den_id, owner);
    den.development_level = 50;
    den.anarchy_level = 50;
    den.infomorphs = 10;
    snap.sov_runtime.mercenary_dens.insert(den_id, den);

    let act_uuid = vec![0xAA; 16];
    let act_hex = hex(&act_uuid);
    let act = PersistedMercenaryActivity {
        uuid: act_uuid.clone(),
        den_id,
        solar_system_id: 3000_0142,
        dungeon_id: 1,
        started: true,
        started_by: 0, // <= 0 -> falls back to den.owner_char_id
        expiry_unix: 1_700_001_000,
        development_impact: 0, // defaults to 10
        anarchy_impact: 0,     // defaults to -10
        infomorph_bonus: 25,
    };

    // Matching MTO site ball + unrelated site ball
    let matching_ball = 8_000_001_i64;
    let unrelated_ball = 8_000_002_i64;
    for (bid, k) in [(matching_ball, act_hex.clone()), (unrelated_ball, "deadbeef".to_string())] {
        snap.sov_runtime.mto_site_balls.insert(bid, k);
        snap.item_positions.insert(bid, [1.0, 2.0, 3.0]);
        snap.items.insert(
            bid,
            InventoryItemRecord {
                item_id: bid,
                type_id: 85_230,
                owner_id: owner,
                location_id: 3000_0142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    }

    let pending: PendingNotices = Default::default();
    complete_mercenary_activity_in_snap(&mut snap, &pending, &act, den_id, 3000_0142);

    let den_after = snap.sov_runtime.mercenary_dens.get(&den_id).unwrap();
    assert_eq!(den_after.development_level, 60);
    assert_eq!(den_after.anarchy_level, 40);
    assert!(den_after.infomorphs > 10);
    assert!(!snap.sov_runtime.mto_site_balls.contains_key(&matching_ball));
    assert!(!snap.items.contains_key(&matching_ball));
    assert!(!snap.item_positions.contains_key(&matching_ball));
    assert!(snap.sov_runtime.mto_site_balls.contains_key(&unrelated_ball));
    assert!(snap.items.contains_key(&unrelated_ball));

    // When development_level >= 100, applied dev impact is 0 and infomorph bonus >= 350 total
    let den_capped_id = 900_003_i64;
    let mut den_capped = sample_den(den_capped_id, owner);
    den_capped.development_level = 100;
    den_capped.infomorphs = 0;
    snap.sov_runtime.mercenary_dens.insert(den_capped_id, den_capped);
    let act2 = PersistedMercenaryActivity {
        uuid: vec![0xBB; 16],
        den_id: den_capped_id,
        solar_system_id: 3000_0142,
        dungeon_id: 1,
        started: true,
        started_by: owner,
        expiry_unix: 1_700_001_000,
        development_impact: 15,
        anarchy_impact: -5,
        infomorph_bonus: 400,
    };
    complete_mercenary_activity_in_snap(&mut snap, &pending, &act2, den_capped_id, 3000_0142);
    let den_capped_after = snap.sov_runtime.mercenary_dens.get(&den_capped_id).unwrap();
    assert_eq!(den_capped_after.development_level, 100);
    assert!(den_capped_after.infomorphs >= 350);
}

#[test]
fn refresh_activities_ignores_unknown_den_and_cleans_up_expired_started_activity() {
    let mut snap = WorldStateSnapshot::default();
    let pending: PendingNotices = Default::default();

    // Unknown den -> no-op
    refresh_activities(&mut snap, &pending, 999_999, 1_700_000_000);
    assert!(pending.read().unwrap().is_empty());

    let den_id = 900_004_i64;
    let mut den = sample_den(den_id, 2110_001);
    den.simulated_at_unix = 1_700_000_000;
    den.next_activity_generation_unix = 1_700_050_000;
    snap.sov_runtime.mercenary_dens.insert(den_id, den);

    let exp_uuid = vec![0x11; 16];
    let exp_hex = hex(&exp_uuid);
    snap.sov_runtime.mercenary_activities.insert(
        exp_hex.clone(),
        PersistedMercenaryActivity {
            uuid: exp_uuid,
            den_id,
            solar_system_id: 3000_0142,
            dungeon_id: 1,
            started: true,
            started_by: 2110_001,
            expiry_unix: 1_700_000_500, // expired at now=1_700_000_600
            development_impact: 10,
            anarchy_impact: -5,
            infomorph_bonus: 50,
        },
    );
    let ball_id = 8_000_010_i64;
    snap.sov_runtime.mto_site_balls.insert(ball_id, exp_hex.clone());
    snap.item_positions.insert(ball_id, [0.0, 0.0, 0.0]);
    snap.items.insert(
        ball_id,
        InventoryItemRecord {
            item_id: ball_id,
            type_id: 85_230,
            owner_id: 2110_001,
            location_id: 3000_0142,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        },
    );

    refresh_activities(&mut snap, &pending, den_id, 1_700_000_600);
    assert!(!snap.sov_runtime.mercenary_activities.contains_key(&exp_hex));
    assert!(!snap.sov_runtime.mto_site_balls.contains_key(&ball_id));
    assert!(!snap.items.contains_key(&ball_id));
    assert!(!snap.item_positions.contains_key(&ball_id));
    assert!(
        pending.read().unwrap().iter().any(|(_, a)| a.type_url.ends_with("RemovedNotice")),
        "expected RemovedNotice on expired started activity"
    );
}

#[test]
fn admin_modify_development_and_anarchy_guards_and_notices() {
    let (db, router) = setup();
    let player = sess(Some(1), Some(2110_001), 0);
    let admin = sess(Some(2), Some(2110_002), service_roles::ROLE_ADMIN);
    let den_id = 900_010_i64;
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_dens
            .insert(den_id, sample_den(den_id, 2110_001));
    });

    for ep in [
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest",
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
    ] {
        // Non-admin -> 403
        assert_eq!(call(&router, &player, ep, Vec::new(), 1_700_000_000).status_code, STATUS_FORBIDDEN);
        // Missing den_id -> 400
        let mut only_lvl = Vec::new();
        encode_varint_field(2, 50, &mut only_lvl);
        assert_eq!(call(&router, &admin, ep, only_lvl, 1_700_000_000).status_code, STATUS_BAD_REQUEST);
        // Level > 100 -> 400
        let mut over = Vec::new();
        encode_seq(1, den_id as u64, &mut over);
        encode_varint_field(2, 101, &mut over);
        assert_eq!(call(&router, &admin, ep, over, 1_700_000_000).status_code, STATUS_BAD_REQUEST);
        // Unknown den -> 404
        let mut missing = Vec::new();
        encode_seq(1, 999_888, &mut missing);
        encode_varint_field(2, 50, &mut missing);
        assert_eq!(call(&router, &admin, ep, missing, 1_700_000_000).status_code, STATUS_NOT_FOUND);

        // Valid -> 200 + EvolutionStateChangedNotice & DefinitionsUpdatedNotice
        router.routing_cache.write().unwrap().entries.clear();
        let mut ok_req = Vec::new();
        encode_seq(1, den_id as u64, &mut ok_req);
        encode_varint_field(2, 85, &mut ok_req);
        assert_eq!(call(&router, &admin, ep, ok_req, 1_700_000_000).status_code, STATUS_OK);
        let urls: Vec<String> = router
            .routing_cache
            .read()
            .unwrap()
            .entries
            .iter()
            .filter_map(|e| e.notice.payload.as_ref().map(|a| a.type_url.clone()))
            .collect();
        assert!(urls.iter().any(|u| u.ends_with("EvolutionStateChangedNotice")));
        assert!(urls.iter().any(|u| u.ends_with("DefinitionsUpdatedNotice")));
    }

    let final_den = db.with_snapshot(|s| s.sov_runtime.mercenary_dens.get(&den_id).cloned().unwrap());
    assert_eq!(final_den.development_level, 85);
    assert_eq!(final_den.anarchy_level, 85);
}

#[test]
fn player_endpoints_auth_skill_clamp_skyhook_field6_and_activity_start_edges() {
    let (db, router) = setup();
    let unauth = sess(Some(1), None, 0);
    let no_char_ctx = AuthoritativeContext::for_session_at(Some(1), None, None, 0, "tranquility", vec![], 1_700_000_000);
    for ep in [
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
    ] {
        assert_eq!(call(&router, &unauth, ep, Vec::new(), 1_700_000_000).status_code, STATUS_FORBIDDEN, "{ep}");
        assert_eq!(
            router.handlers.read().unwrap()[ep](&no_char_ctx, &GoogleAny::default(), &DynamicQuasarMessage::new(ep))
                .unwrap_err()
                .0,
            STATUS_UNAUTHORIZED,
            "{ep} handler"
        );
    }

    let cid = 2110_020_i64;
    let den_id = 900_020_i64;
    let skyhook_id = 1_000_000_555_i64;
    let act_uuid = vec![0x55; 16];
    db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&90_000_001].clone();
        snap.characters.entry(cid).or_insert_with(|| {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = 2110_020;
            ch.name = "MercDenTester".to_string();
            ch.corporation_id = 98_000_001;
            ch.solar_system_id = 3000_0142;
            ch.station_id = None;
            ch.structure_id = None;
            ch
        });
        snap.skills.entry(cid).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 9, // > 5 -> must clamp to 5
                skill_points: 256_000.0,
            },
        );
        let mut den = sample_den(den_id, cid);
        den.simulated_at_unix = 1_700_000_000;
        snap.sov_runtime.mercenary_dens.insert(den_id, den);
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: 81_080,
                owner_corp_id: 98_000_001,
                solar_system_id: 3000_0142,
                name: "Planet Skyhook".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.mercenary_activities.insert(
            hex(&act_uuid),
            PersistedMercenaryActivity {
                uuid: act_uuid.clone(),
                den_id,
                solar_system_id: 3000_0142,
                dungeon_id: 1,
                started: true, // already started -> 409
                started_by: cid,
                expiry_unix: 1_700_100_000,
                development_impact: 5,
                anarchy_impact: -5,
                infomorph_bonus: 10,
            },
        );
    });

    let p = sess(Some(20), Some(cid), 0);

    // GetMaximumForCharacter clamps 9 to 5
    let max_resp = call(&router, &p, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new(), 1_700_000_000);
    assert_eq!(max_resp.status_code, STATUS_OK);
    let max_wire = WireMessage::decode(&max_resp.payload.unwrap().value).unwrap();
    let cur_max = max_wire
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_varint())
        .expect("missing current max");
    assert_eq!(cur_max, 5);

    // GetAsOwner emits field 6 when skyhook_id is present in snap.structures
    let mut owner_req = Vec::new();
    encode_seq(1, den_id as u64, &mut owner_req);
    let owner_resp = call(&router, &p, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", owner_req, 1_700_000_000);
    assert_eq!(owner_resp.status_code, STATUS_OK);
    let owner_wire = WireMessage::decode(&owner_resp.payload.unwrap().value).unwrap();
    assert!(
        owner_wire.fields.iter().any(|f| f.field_number == 6),
        "field 6 (skyhook_owner_corp) must be emitted when skyhook is in snap.structures"
    );

    // StartRequest unknown uuid -> 404
    let mut unk_id = Vec::new();
    encode_bytes_field(1, &vec![0x99; 16], &mut unk_id);
    let mut unk_start = Vec::new();
    encode_bytes_field(1, &unk_id, &mut unk_start);
    assert_eq!(
        call(&router, &p, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", unk_start, 1_700_000_000).status_code,
        STATUS_NOT_FOUND
    );

    // StartRequest already started -> 409
    let mut dup_id = Vec::new();
    encode_bytes_field(1, &act_uuid, &mut dup_id);
    let mut dup_start = Vec::new();
    encode_bytes_field(1, &dup_id, &mut dup_start);
    assert_eq!(
        call(&router, &p, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", dup_start, 1_700_000_000).status_code,
        STATUS_CONFLICT
    );
}
