//! Deep branch-coverage and mutation-killing unit tests for `quasar_sov_mercden` (`Q-8`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_sov_hub::PendingNotices;
use crate::services::quasar_sov_mercden::*;
use eve_db::{
    InventoryItemRecord, IskCents, PersistedMercenaryActivity, PersistedMercenaryDen,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock, RwLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn id_msg(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn act_start_msg(uuid: Vec<u8>) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("ActId");
    inner.fields.insert(1, DynamicValue::Bytes(uuid));
    let mut outer = DynamicQuasarMessage::new("StartReq");
    outer.fields.insert(1, DynamicValue::Message(Box::new(inner)));
    outer
}

fn invoke_mercden(
    router: &PublicGatewayRouter,
    req_type: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(req_type)
        .cloned()
        .unwrap_or_else(|| panic!("missing handler for {req_type}"));
    handler(ctx, &GoogleAny::default(), msg)
}

#[test]
fn stage_boundaries_long_simulation_remainder_and_disabled_encoding_branches() {
    // Exact boundary checks for stage_for_level
    for (lvl, expected_stage) in [
        (0, 1),
        (19, 1),
        (20, 2),
        (39, 2),
        (40, 3),
        (69, 3),
        (70, 4),
        (99, 4),
        (100, 5),
        (250, 5),
    ] {
        assert_eq!(stage_for_level(lvl), expected_stage, "level {lvl}");
    }

    let mut den = PersistedMercenaryDen {
        den_id: 91_100_001,
        owner_char_id: 90_000_001,
        skyhook_id: 79_000_001,
        solar_system_id: 30_000_142,
        planet_id: 40_009_080,
        type_id: 85_230,
        enabled: false,
        cargo_extraction_enabled: false,
        development_level: 15,
        anarchy_level: 15,
        infomorphs: 0,
        simulated_at_unix: 0,
        next_activity_generation_unix: 1_000,
    };

    // 1. Disabled den encodes field 1 (not field 2) in encode_evolution_simulation
    let sim_disabled = WireMessage::decode(&encode_evolution_simulation(&den)).unwrap();
    assert_eq!(sim_disabled.fields[0].field_number, 1);

    // 2. simulate_den_with_rem when simulated_at_unix == 0 or !enabled sets simulated_at_unix = now and returns false
    let mut rem = 0u64;
    assert!(!simulate_den_with_rem(&mut den, &mut rem, 5_000));
    assert_eq!(den.simulated_at_unix, 5_000);

    den.enabled = true;
    let sim_enabled = WireMessage::decode(&encode_evolution_simulation(&den)).unwrap();
    assert_eq!(sim_enabled.fields[0].field_number, 2);

    // 3. now <= simulated_at_unix or < 1 hour elapsed returns false without advancing simulated_at_unix
    assert!(!simulate_den_with_rem(&mut den, &mut rem, 4_000));
    assert_eq!(den.simulated_at_unix, 5_000);
    assert!(!simulate_den_with_rem(&mut den, &mut rem, 5_000 + 3_599));
    assert_eq!(den.simulated_at_unix, 5_000);

    // 4. Two 12-hour steps accumulate evo_rem_secs across calls to trigger a 24h evolution step (+5 dev/anarchy)
    assert!(!simulate_den_with_rem(
        &mut den,
        &mut rem,
        5_000 + 12 * INFOMORPH_UNIT_SECS
    ));
    assert_eq!(den.development_level, 15);
    assert_eq!(rem, 12 * INFOMORPH_UNIT_SECS);

    // Second 12h crosses 24h -> dev 15 -> 20 (stage 1 -> 2, returns true!)
    assert!(simulate_den_with_rem(
        &mut den,
        &mut rem,
        5_000 + 24 * INFOMORPH_UNIT_SECS
    ));
    assert_eq!(den.development_level, 20);
    assert_eq!(den.anarchy_level, 20);
    assert_eq!(rem, 0);

    // 5. Long simulation > 720 hours (800 hours) exercises extra_hours branch and caps infomorphs at 3000 & levels at 100
    let t_after_24h = den.simulated_at_unix;
    assert!(simulate_den_with_rem(
        &mut den,
        &mut rem,
        t_after_24h + 800 * INFOMORPH_UNIT_SECS
    ));
    assert_eq!(den.infomorphs, INFOMORPH_CARGO_CAPACITY);
    assert_eq!(den.development_level, 100);
    assert_eq!(den.anarchy_level, 100);
}

#[test]
fn complete_mercenary_activity_and_refresh_activities_branches() {
    let db = WorldRepository::with_demo_seed();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let owner_id = 90_000_001i64;
    let den_id = 91_100_010i64;
    let sys_id = 30_000_142u32;

    db.with_mut_snapshot(|snap| {
        // Unknown den in simulate_den_in_snap and refresh_activities is a no-op
        assert!(!simulate_den_in_snap(&mut snap.sov_runtime, 999_999, 10_000));
        refresh_activities(snap, &pending, 999_999, 10_000);

        let initial_bal = snap.characters[&owner_id].balance;
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: owner_id,
                skyhook_id: 79_000_001,
                solar_system_id: sys_id,
                planet_id: 40_009_080,
                type_id: 85_230,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 100, // At MAX_LEVEL (100)
                anarchy_level: 25,
                infomorphs: 100,
                simulated_at_unix: 1_000,
                next_activity_generation_unix: 2_000,
            },
        );

        // Activity with started_by = 0 (falls back to den.owner_char_id), dev_impact = 0, anarchy_impact = 0, bonus = 10
        let uuid1 = vec![0xAA; 16];
        let hex1 = hex(&uuid1);
        let ball_id_1 = 880_001i64;
        let ball_id_keep = 880_002i64;
        snap.items.insert(
            ball_id_1,
            InventoryItemRecord {
                item_id: ball_id_1,
                type_id: 24_690,
                owner_id: 500_010,
                location_id: sys_id as i64,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ball_id_1, [1.0, 2.0, 3.0]);
        snap.sov_runtime.mto_site_balls.insert(ball_id_1, hex1);
        snap.sov_runtime
            .mto_site_balls
            .insert(ball_id_keep, "other_uuid".into());

        let act_max = PersistedMercenaryActivity {
            uuid: uuid1,
            den_id,
            solar_system_id: sys_id,
            dungeon_id: 0, // dungeon_id == 0 branch in encode_activity_attributes
            started: true,
            started_by: 0,
            expiry_unix: 5_000,
            development_impact: 0,
            anarchy_impact: 0,
            infomorph_bonus: 10,
        };
        assert!(!encode_activity_attributes(&act_max).is_empty());

        complete_mercenary_activity_in_snap(snap, &pending, &act_max, den_id, sys_id);
        let d = &snap.sov_runtime.mercenary_dens[&den_id];
        // At MAX_LEVEL (100), infomorph_bonus is max(10, 350) = 350 -> 100 + 350 = 450; anarchy reduced by default -10 -> 15
        assert_eq!(d.infomorphs, 450);
        assert_eq!(d.development_level, 100);
        assert_eq!(d.anarchy_level, 15);
        // Owner received 7,500,000 ISK reward
        assert_eq!(
            snap.characters[&owner_id].balance,
            initial_bal.checked_add(IskCents::from_isk(7_500_000)).unwrap()
        );
        // Site ball 880_001 cleaned up; 880_002 preserved
        assert!(!snap.items.contains_key(&ball_id_1));
        assert!(snap.sov_runtime.mto_site_balls.contains_key(&ball_id_keep));

        // Now test refresh_activities:
        // Insert an expired started activity with a site ball -> refresh_activities removes it and cleans up its ball
        let exp_uuid = vec![0xBB; 16];
        let exp_hex = hex(&exp_uuid);
        let exp_ball = 880_003i64;
        snap.items.insert(
            exp_ball,
            InventoryItemRecord {
                item_id: exp_ball,
                type_id: 24_690,
                owner_id: 500_010,
                location_id: sys_id as i64,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .mto_site_balls
            .insert(exp_ball, exp_hex.clone());
        snap.sov_runtime.mercenary_activities.insert(
            exp_hex.clone(),
            PersistedMercenaryActivity {
                uuid: exp_uuid,
                den_id,
                solar_system_id: sys_id,
                dungeon_id: 55,
                started: true,
                started_by: owner_id,
                expiry_unix: 1_500,
                development_impact: 10,
                anarchy_impact: -10,
                infomorph_bonus: 350,
            },
        );

        // Call refresh_activities at t=1_800 (< next_activity_generation_unix=2_000):
        // Expires exp_hex and cleans up exp_ball, but does NOT generate new activities yet!
        refresh_activities(snap, &pending, den_id, 1_800);
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&exp_hex));
        assert!(!snap.items.contains_key(&exp_ball));
        assert!(snap.sov_runtime.mercenary_activities.is_empty());

        // Call refresh_activities at t=2_500 (>= 2_000): generates 7 activities (stage 5 capacity = 7)
        refresh_activities(snap, &pending, den_id, 2_500);
        assert_eq!(snap.sov_runtime.mercenary_activities.len(), 7);
    });
}

#[test]
fn rpc_authorization_reinforcement_skyhook_and_start_activity_error_matrix() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db));

    let owner_id = 90_000_001i64;
    let other_id = 90_000_002i64;
    let corp_id = 98_400_001i64;
    let den_id = 91_200_001i64;
    let skyhook_id = 79_200_001i64;
    let sys_id = 30_000_142u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        // Train Mercenary Den Management to level 8 (clamps to 5)
        snap.skills.entry(owner_id).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 8,
                skill_points: 256_000.0,
            },
        );
        // Host skyhook structure so GetAsOwnerResponse encodes field 6 (owner_corp_id)
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: 81_080,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Skyhook".into(),
                state: "ShieldVulnerable".into(),
                position: [100_000.0, 0.0, 0.0],
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: owner_id,
                skyhook_id,
                solar_system_id: sys_id,
                planet_id: 40_009_080,
                type_id: 85_230,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 50,
                anarchy_level: 10,
                infomorphs: 100,
                simulated_at_unix: 1_000,
                next_activity_generation_unix: 0,
            },
        );
    });

    // 1. Unauthenticated context rejected with STATUS_UNAUTHORIZED on all character endpoints
    let anon_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for rpc in [
        "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
    ] {
        assert_eq!(
            invoke_mercden(&router, rpc, &anon_ctx, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_UNAUTHORIZED
        );
    }

    let owner_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(owner_id as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
        10_000,
    );
    let other_ctx = AuthoritativeContext::for_session_at(
        Some(2),
        Some(other_id as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
        10_000,
    );

    // 2. Non-admin rejected on ModifyDevelopmentLevelRequest; admin rejected on missing den (STATUS_NOT_FOUND) and negative level (STATUS_BAD_REQUEST)
    assert!(invoke_mercden(
        &router,
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest",
        &owner_ctx,
        &id_msg(1, den_id),
    )
    .is_err());

    let mut gm_ctx = owner_ctx.clone();
    gm_ctx.service_role_mask = u64::MAX;
    let mut neg_lvl = id_msg(1, den_id);
    neg_lvl.fields.insert(2, DynamicValue::Int64(-5));
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest",
            &gm_ctx,
            &neg_lvl,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut missing_den_lvl = id_msg(1, 999_888_777);
    missing_den_lvl.fields.insert(2, DynamicValue::Int64(50));
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
            &gm_ctx,
            &missing_den_lvl,
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 3. GetAsOwnerRequest: missing den -> NOT_FOUND; non-owner -> FORBIDDEN; reinforced <50% shield -> CONFLICT; >=50% shield -> Ok with skyhook owner field 6
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            &owner_ctx,
            &id_msg(1, 999_888_777),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            &other_ctx,
            &id_msg(1, den_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (1_000, 2_000, 49));
    });
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            &owner_ctx,
            &id_msg(1, den_id),
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );

    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (1_000, 2_000, 50));
    });
    let owner_resp = invoke_mercden(
        &router,
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        &owner_ctx,
        &id_msg(1, den_id),
    )
    .unwrap();
    let owner_wm = WireMessage::decode(&owner_resp.value).unwrap();
    assert!(owner_wm.fields.iter().any(|f| f.field_number == 6));

    // 4. StartRequest error guards: empty UUID, missing activity, wrong owner, docked/wrong system, expired activity
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            &owner_ctx,
            &empty_msg,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            &owner_ctx,
            &act_start_msg(vec![0x11; 16]),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // Seed an activity expiring at t=5_000 (already expired at owner_ctx now=10_000)
    let act_uuid = vec![0xCC; 16];
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_activities.insert(
            hex(&act_uuid),
            PersistedMercenaryActivity {
                uuid: act_uuid.clone(),
                den_id,
                solar_system_id: sys_id,
                dungeon_id: 0,
                started: false,
                started_by: 0,
                expiry_unix: 5_000,
                development_impact: 10,
                anarchy_impact: -10,
                infomorph_bonus: 350,
            },
        );
    });
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            &other_ctx,
            &act_start_msg(act_uuid.clone()),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            &owner_ctx,
            &act_start_msg(act_uuid.clone()),
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );

    // Dock character in station -> STATUS_FORBIDDEN even when expiry_unix > now
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_activities
            .get_mut(&hex(&act_uuid))
            .unwrap()
            .expiry_unix = 50_000;
        snap.characters.get_mut(&owner_id).unwrap().station_id = Some(60_003_760);
    });
    assert_eq!(
        invoke_mercden(
            &router,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            &owner_ctx,
            &act_start_msg(act_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
}
