//! Extra unit and mutation-killing tests for `services::quasar_cosmetics_skinr_seq`.

#![forbid(unsafe_code)]

use crate::services::quasar_cosmetics_skinr::{
    encode_component_id, encode_layout_slot, encode_skin_id, encode_skin_license_id,
    FiniteComponentLicense, SequencingJobRecord, SkinDesignRecord, SkinLayoutSlot,
    SkinrCosmeticsState,
};
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(|| {
        if let Ok(reg) = QuasarDescriptorRegistry::open_default() {
            return reg;
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap();
        let manifest = root.join("data/quasar_descriptor_manifest.json");
        for anc in root.ancestors().skip(1) {
            let db = anc.join("data/quasar_descriptors.sqlite");
            if db.exists() {
                if let Ok(reg) = QuasarDescriptorRegistry::open(&db, &manifest) {
                    return Arc::new(reg);
                }
            }
        }
        QuasarDescriptorRegistry::open_default_or_empty()
    }))
}

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
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
            correlation_uuid: vec![21; 16],
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

#[track_caller]
fn assert_code(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
    code: u32,
) {
    let rsp = call(router, s, ty, value, now);
    assert_eq!(rsp.status_code, code, "{ty}: {}", rsp.status_message);
}

#[test]
fn skin_get_and_license_endpoints_error_and_edge_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_cosmetics_skinr_seq::register(&router, Arc::clone(&skinr));
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;
    let anon = sess(Some(1), None, service_roles::ROLE_PLAYER_DEFAULT);
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let other = sess(Some(2), Some(other_cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(2), Some(other_cid), service_roles::ROLE_GM_DEFAULT);
    let gm_p1 = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);

    // 1. handle_skin_get: empty -> 400, unknown -> 404, unsequenced vs sequenced design attributes
    let get_skin_ep = "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest";
    assert_code(&router, &player, get_skin_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    let mut nf_req = Vec::new();
    encode_bytes_field(1, &encode_skin_id("missing_skin"), &mut nf_req);
    assert_code(&router, &player, get_skin_ep, nf_req, now, STATUS_NOT_FOUND);

    // Insert a design with slots and first_sequenced_unix = None, plus numeric-named skin_0000002a
    skinr.data.write().unwrap().skins.insert(
        "skin_0000002a".into(),
        SkinDesignRecord {
            skin_hex: "skin_0000002a".into(),
            name: "Unsequenced Design".into(),
            line_name: "Test Line".into(),
            creator_id: cid as u32,
            ship_type_id: 587,
            pattern_blend_mode: 2,
            slots: vec![
                SkinLayoutSlot {
                    slot_id: 1,
                    component_id: 10,
                    is_pattern: false,
                    primary_coating_id: None,
                    secondary_coating_id: None,
                },
                SkinLayoutSlot {
                    slot_id: 5,
                    component_id: 20,
                    is_pattern: true,
                    primary_coating_id: Some(10),
                    secondary_coating_id: Some(11),
                },
            ],
            tier_level: 3,
            first_sequenced_unix: None,
        },
    );
    let mut num_inner = Vec::new();
    encode_varint_field(1, 0x2a, &mut num_inner);
    let mut num_req = Vec::new();
    encode_bytes_field(1, &num_inner, &mut num_req);
    assert_code(&router, &player, get_skin_ep, num_req, now, STATUS_OK);

    // 2. GetOwnedRequest: unauthenticated -> 403, eve. prefix with 0 licenses encodes cid at field 1
    assert_code(
        &router,
        &anon,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
        now,
        STATUS_FORBIDDEN,
    );
    let empty_owned_eve = call(
        &router,
        &player,
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
        now,
    );
    assert_eq!(empty_owned_eve.status_code, STATUS_OK);
    assert!(!empty_owned_eve.payload.unwrap().value.is_empty());

    // 3. Admin grant validation: non-admin -> 403, missing hex -> 400, qty=0 -> 400, double grant -> UpdatedNotice
    let grant_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest";
    assert_code(&router, &player, grant_ep, Vec::new(), now, STATUS_FORBIDDEN);
    assert_code(&router, &gm, grant_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    let mut zero_grant = Vec::new();
    encode_bytes_field(1, &encode_skin_id("skin_double"), &mut zero_grant);
    assert_code(&router, &gm, grant_ep, zero_grant.clone(), now, STATUS_BAD_REQUEST);
    let mut ok_grant = zero_grant;
    encode_varint_field(2, 1, &mut ok_grant);
    assert_code(&router, &gm_p1, grant_ep, ok_grant.clone(), now, STATUS_OK);
    assert_code(&router, &gm_p1, grant_ep, ok_grant, now, STATUS_OK);

    // 4. License GetRequest: missing id -> 400, not found -> 404, admin reading other char's license -> 200
    let lic_get_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest";
    assert_code(&router, &player, lic_get_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    let mut nf_lic = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(cid as u32, "no_such_skin"), &mut nf_lic);
    assert_code(&router, &player, lic_get_ep, nf_lic, now, STATUS_NOT_FOUND);
    let mut p1_lic = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(cid as u32, "skin_double"), &mut p1_lic);
    assert_code(
        &router,
        &gm,
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        p1_lic.clone(),
        now,
        STATUS_OK,
    );

    // 5. ActivateRequest: missing id -> 400, other char -> 403, valid -> 200, double activate -> 400
    let act_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest";
    assert_code(&router, &player, act_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    assert_code(&router, &other, act_ep, p1_lic.clone(), now, STATUS_FORBIDDEN);
    assert_code(&router, &player, act_ep, p1_lic.clone(), now, STATUS_OK);
    assert_code(&router, &player, act_ep, p1_lic.clone(), now, STATUS_BAD_REQUEST);

    // 6. Admin RevokeRequest: non-admin -> 403, missing id -> 400, valid -> 200, second revoke -> 404
    let rev_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest";
    assert_code(&router, &player, rev_ep, p1_lic.clone(), now, STATUS_FORBIDDEN);
    assert_code(&router, &gm_p1, rev_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    assert_code(&router, &gm_p1, rev_ep, p1_lic.clone(), now, STATUS_OK);
    assert_code(&router, &gm_p1, rev_ep, p1_lic, now, STATUS_NOT_FOUND);
}

#[test]
fn apply_and_unapply_validation_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let other = sess(Some(2), Some(other_cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);

    // Seed items: non-ship item (Tritanium 34), other char's ship (587), and wrong-hull ship (Punisher 597)
    db.with_mut_snapshot(|s| {
        for (item_id, type_id, owner_id) in [
            (700_001, 34, cid),
            (700_002, 587, other_cid),
            (700_003, 597, cid),
        ] {
            s.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: -1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // Grant & activate "skin_rifter_only" (ship_type_id = 587)
    let mut grant_req = Vec::new();
    encode_bytes_field(1, &encode_skin_id("skin_rifter_only"), &mut grant_req);
    encode_varint_field(2, 1, &mut grant_req);
    call(
        &router,
        &gm,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        grant_req,
        now,
    );
    let mut lic_req = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(cid as u32, "skin_rifter_only"), &mut lic_req);
    call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
        lic_req.clone(),
        now,
    );

    let mk_apply = |base: &[u8], ship_id: u64| {
        let mut r = base.to_vec();
        if ship_id > 0 {
            let mut sh = Vec::new();
            encode_varint_field(1, ship_id, &mut sh);
            encode_bytes_field(2, &sh, &mut r);
        }
        r
    };

    let apply_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest";
    let unapply_ep = "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest";

    assert_code(&router, &player, apply_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    assert_code(&router, &other, apply_ep, mk_apply(&lic_req, 700_002), now, STATUS_FORBIDDEN);
    assert_code(&router, &player, apply_ep, mk_apply(&lic_req, 0), now, STATUS_BAD_REQUEST);
    assert_code(&router, &player, apply_ep, mk_apply(&lic_req, 999_999_999), now, STATUS_NOT_FOUND);
    assert_code(&router, &player, apply_ep, mk_apply(&lic_req, 700_002), now, STATUS_FORBIDDEN);
    assert_code(&router, &player, apply_ep, mk_apply(&lic_req, 700_001), now, STATUS_BAD_REQUEST);
    assert_code(&router, &player, apply_ep, mk_apply(&lic_req, 700_003), now, STATUS_BAD_REQUEST);

    assert_code(&router, &other, unapply_ep, mk_apply(&lic_req, 700_002), now, STATUS_FORBIDDEN);
    assert_code(&router, &player, unapply_ep, mk_apply(&lic_req, 0), now, STATUS_BAD_REQUEST);
    assert_code(&router, &player, unapply_ep, mk_apply(&lic_req, 999_999_999), now, STATUS_NOT_FOUND);
    assert_code(&router, &player, unapply_ep, mk_apply(&lic_req, 700_002), now, STATUS_FORBIDDEN);
}

#[test]
fn sequencing_job_initiate_validation_finite_licenses_plex_rollback_and_settle() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_cosmetics_skinr_seq::register(&router, Arc::clone(&skinr));
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let other = sess(Some(2), Some(other_cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(2), Some(other_cid), service_roles::ROLE_GM_DEFAULT);

    let pat_slot = *skinr
        .catalog
        .allowed_slots_for_ship(587)
        .iter()
        .find(|&&s| {
            skinr
                .catalog
                .slot_allowed_categories
                .get(&s)
                .is_some_and(|cats| cats.contains(&2))
        })
        .unwrap();
    let coat_id = *skinr
        .catalog
        .components
        .iter()
        .find(|(_, c)| {
            !c.is_pattern()
                && !c.disallowed_slots.contains(&1)
                && skinr
                    .catalog
                    .slot_allowed_categories
                    .get(&1)
                    .is_none_or(|cats| cats.contains(&c.category))
        })
        .unwrap()
        .0;
    let pat_id = *skinr
        .catalog
        .components
        .iter()
        .find(|(_, c)| c.is_pattern() && !c.disallowed_slots.contains(&pat_slot))
        .unwrap()
        .0;

    let init_ep = "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest";
    assert_code(&router, &player, init_ep, Vec::new(), now, STATUS_BAD_REQUEST);

    let mut bad_qty = Vec::new();
    encode_varint_field(5, 101, &mut bad_qty);
    assert_code(&router, &player, init_ep, bad_qty, now, STATUS_BAD_REQUEST);

    let mut bad_ship_lay = Vec::new();
    let mut sh34 = Vec::new();
    encode_varint_field(1, 34, &mut sh34);
    encode_bytes_field(2, &sh34, &mut bad_ship_lay);
    let mut bad_ship_req = Vec::new();
    encode_bytes_field(3, &bad_ship_lay, &mut bad_ship_req);
    assert_code(&router, &player, init_ep, bad_ship_req, now, STATUS_BAD_REQUEST);

    let mut sh587 = Vec::new();
    encode_varint_field(1, 587, &mut sh587);
    for (slot_id, comp_id, is_pat) in [
        (99, coat_id, false),
        (pat_slot, coat_id, false),
        (1, pat_id, false),
        (1, 999_999, false),
    ] {
        let mut lay = Vec::new();
        encode_bytes_field(
            1,
            &encode_layout_slot(&SkinLayoutSlot {
                slot_id,
                component_id: comp_id,
                is_pattern: is_pat,
                primary_coating_id: None,
                secondary_coating_id: None,
            }),
            &mut lay,
        );
        encode_bytes_field(2, &sh587, &mut lay);
        let mut req = Vec::new();
        encode_bytes_field(3, &lay, &mut req);
        assert_code(&router, &player, init_ep, req, now, STATUS_BAD_REQUEST);
    }

    let mut valid_lay = Vec::new();
    encode_bytes_field(
        1,
        &encode_layout_slot(&SkinLayoutSlot {
            slot_id: 1,
            component_id: coat_id,
            is_pattern: false,
            primary_coating_id: None,
            secondary_coating_id: None,
        }),
        &mut valid_lay,
    );
    encode_bytes_field(
        1,
        &encode_layout_slot(&SkinLayoutSlot {
            slot_id: pat_slot,
            component_id: pat_id,
            is_pattern: true,
            primary_coating_id: None,
            secondary_coating_id: None,
        }),
        &mut valid_lay,
    );
    encode_bytes_field(2, &sh587, &mut valid_lay);
    let mut valid_init_req = Vec::new();
    encode_bytes_field(3, &valid_lay, &mut valid_init_req);
    encode_varint_field(5, 2, &mut valid_init_req);
    db.with_mut_snapshot(|s| {
        s.skills.entry(cid).or_default().insert(
            81375,
            eve_db::TrainedSkillRecord {
                type_id: 81375,
                level: 1,
                skill_points: 1_000.0,
            },
        );
    });

    // Insufficient component license capacity -> 400
    assert_code(&router, &player, init_ep, valid_init_req.clone(), now, STATUS_BAD_REQUEST);

    // Seed finite licenses (capacity = 3), PLEX = 0 -> PLEX failure rolls back finite capacity!
    {
        let mut data = skinr.data.write().unwrap();
        for (comp_id, is_pattern) in [(coat_id, false), (pat_id, true)] {
            data.finite_licenses.insert(
                SkinrCosmeticsState::comp_key(cid as u32, is_pattern, comp_id),
                FiniteComponentLicense {
                    character_id: cid as u32,
                    component_id: comp_id,
                    is_pattern,
                    capacity: 3,
                    first_acquired_unix: now,
                },
            );
        }
    }
    db.with_mut_snapshot(|s| s.characters.get_mut(&cid).unwrap().plex_balance = i64::MIN);
    assert_code(&router, &player, init_ep, valid_init_req.clone(), now, STATUS_BAD_REQUEST);
    let coat_key = SkinrCosmeticsState::comp_key(cid as u32, false, coat_id);
    assert_eq!(skinr.data.read().unwrap().finite_licenses[&coat_key].capacity, 3);

    // Give PLEX = 10_000 -> Initiate succeeds and decrements finite license capacity from 3 to 1 (qty=2)
    db.with_mut_snapshot(|s| s.characters.get_mut(&cid).unwrap().plex_balance = 10_000);
    assert_code(&router, &player, init_ep, valid_init_req, now, STATUS_OK);
    assert_eq!(skinr.data.read().unwrap().finite_licenses[&coat_key].capacity, 1);

    let job_uuid = skinr
        .data
        .read()
        .unwrap()
        .sequencing_jobs
        .values()
        .next()
        .unwrap()
        .job_uuid;
    let mut job_id_msg = Vec::new();
    encode_bytes_field(1, &job_uuid, &mut job_id_msg);
    let mut job_get_req = Vec::new();
    encode_bytes_field(1, &job_id_msg, &mut job_get_req);

    let job_get_ep = "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest";
    let early_ep = "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest";
    assert_code(&router, &player, job_get_ep, Vec::new(), now, STATUS_BAD_REQUEST);
    assert_code(&router, &other, job_get_ep, job_get_req.clone(), now, STATUS_FORBIDDEN);
    assert_code(&router, &gm, job_get_ep, job_get_req.clone(), now, STATUS_OK);

    // CompleteEarly with insufficient PLEX rolls back completed_unix
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            let uid = ch.user_id;
            ch.plex_balance = 0;
            s.account_plex_vault.insert(uid, 0);
        }
    });
    assert_code(&router, &player, early_ep, job_get_req.clone(), now + 10, STATUS_BAD_REQUEST);

    // Advance clock past planned_duration_secs -> settle_completed_jobs completes job and grants 2 licenses
    let settle_now = now + 1_000_000;
    let active_after = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
        settle_now,
    );
    assert!(active_after.payload.unwrap().value.is_empty());
    assert_code(&router, &player, early_ep, job_get_req, settle_now, STATUS_BAD_REQUEST);

    let _ = (
        SequencingJobRecord {
            job_uuid: [0; 16],
            skin_hex: String::new(),
            quantity: 1,
            sequencer_id: 1,
            initiated_unix: 0,
            planned_duration_secs: 600,
            completed_unix: None,
            failed_reason: Some(2),
        },
        TrainedSkillRecord {
            type_id: 81040,
            level: 5,
            skill_points: 256_000.0,
        },
        encode_component_id(coat_id, false),
    );
}

#[test]
fn mutant_kill_skinr_skill_discounts_cap_runs_and_tier_endpoints() {
    use crate::services::quasar_cosmetics_skinr_seq::{max_runs_per_job, skill_discounts_and_cap};

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_cosmetics_skinr_seq::register(&router, Arc::clone(&skinr));

    let cid_no_skills = 90_000_099_i64;
    let cid_mid_a = 90_000_098_i64;
    let cid_mid_b = 90_000_097_i64;
    let cid_max = 90_000_096_i64;
    let add_sk = |map: &mut std::collections::HashMap<u32, TrainedSkillRecord>, tid: u32, lvl: u8| {
        map.insert(
            tid,
            TrainedSkillRecord {
                type_id: tid,
                level: lvl,
                skill_points: 256_000.0,
            },
        );
    };

    db.with_mut_snapshot(|s| {
        let base_char = s.characters[&90_000_001].clone();
        for (i, &c) in [cid_mid_a, cid_mid_b, cid_max].iter().enumerate() {
            s.characters.entry(c).or_insert_with(|| {
                let mut ch = base_char.clone();
                ch.character_id = c;
                ch.user_id = (90_000 + i) as u64;
                ch.name = format!("SkinrSkillTester{i}");
                ch.corporation_id = 1_000_009;
                ch.solar_system_id = 3000_0142;
                ch
            });
        }
        // cid_mid_a: par_bonus=3 (lvl 2 -> 81366), mt_bonus=2 (lvl 1 -> 81367), dur_disc=1+3+6+10=20
        // max_jobs = 1 + 3 + 2 = 6 (whereas 1*3+2=5, 1+3-2=2, 1+3*2=7)
        let ma = s.skills.entry(cid_mid_a).or_default();
        add_sk(ma, 81377, 1);
        add_sk(ma, 81372, 2);
        add_sk(ma, 81373, 3);
        add_sk(ma, 81374, 4);
        add_sk(ma, 81366, 2);
        add_sk(ma, 81367, 1);
        // Batch lvl 5 (49), Mass lvl 5 (250), Industrial lvl 1 (200) -> 1 + 49 + 250 + 200 = 500
        add_sk(ma, 81375, 5);
        add_sk(ma, 81364, 5);
        add_sk(ma, 81365, 1);

        // cid_mid_b: par_bonus=5 (lvl 3), mt_bonus=8 (lvl 4) -> 1 + 5 + 8 = 14
        let mb = s.skills.entry(cid_mid_b).or_default();
        add_sk(mb, 81366, 3);
        add_sk(mb, 81367, 4);
        add_sk(mb, 81377, 5); // dur_disc=15

        // cid_max: all level 5 -> capped at (0, 60, 20) and 5_000 runs
        let mm = s.skills.entry(cid_max).or_default();
        for sid in [81377, 81372, 81373, 81374, 81366, 81367, 81375, 81364, 81365] {
            add_sk(mm, sid, 5);
        }
    });

    assert_eq!(skill_discounts_and_cap(&skinr, None), (0, 0, 1));
    assert_eq!(
        skill_discounts_and_cap(&skinr, Some(cid_no_skills as u32)),
        (0, 0, 1)
    );
    assert_eq!(
        skill_discounts_and_cap(&skinr, Some(cid_mid_a as u32)),
        (0, 20, 6)
    );
    assert_eq!(
        skill_discounts_and_cap(&skinr, Some(cid_mid_b as u32)),
        (0, 15, 14)
    );
    assert_eq!(
        skill_discounts_and_cap(&skinr, Some(cid_max as u32)),
        (0, 60, 20)
    );

    assert_eq!(max_runs_per_job(&skinr, cid_no_skills as u32), 1);
    assert_eq!(max_runs_per_job(&skinr, cid_mid_a as u32), 500);
    assert_eq!(max_runs_per_job(&skinr, cid_max as u32), 5_000);

    // Tier pricing, tier durations, and GetSkillBasedJobsCapacityRequest responses
    let now = 1_760_000_000u64;
    let s_none = sess(Some(1), Some(cid_no_skills), service_roles::ROLE_PLAYER_DEFAULT);
    let s_mid = sess(Some(2), Some(cid_mid_a), service_roles::ROLE_PLAYER_DEFAULT);

    let tp = call(
        &router,
        &s_mid,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
        now,
    );
    assert_eq!(tp.status_code, STATUS_OK);
    assert!(!tp.payload.as_ref().unwrap().value.is_empty());

    let td_base = call(
        &router,
        &s_none,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        Vec::new(),
        now,
    );
    let td_disc = call(
        &router,
        &s_mid,
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterRequest",
        Vec::new(),
        now,
    );
    assert_eq!(td_base.status_code, STATUS_OK);
    assert_eq!(td_disc.status_code, STATUS_OK);
    assert_ne!(
        td_base.payload.as_ref().unwrap().value,
        td_disc.payload.as_ref().unwrap().value
    );

    let cap_rsp = call(
        &router,
        &s_mid,
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
        Vec::new(),
        now,
    );
    assert_eq!(cap_rsp.status_code, STATUS_OK);
    assert!(!cap_rsp.payload.unwrap().value.is_empty());
}
