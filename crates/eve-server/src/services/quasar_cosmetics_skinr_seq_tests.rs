//! Unit tests for `quasar_cosmetics_skinr_seq`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use crate::services::quasar_cosmetics_skinr::SkinrStaticCatalog;
use eve_db::{TrainedSkillRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call(router: &PublicGatewayRouter, s: &SessionState, ty: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![13; 16],
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

#[test]
fn induction_discounts_skill_caps_and_wire_parsers() {
    assert_eq!(induction_time_disc_pct(0), 0);
    assert_eq!(induction_time_disc_pct(1), 1);
    assert_eq!(induction_time_disc_pct(2), 3);
    assert_eq!(induction_time_disc_pct(3), 6);
    assert_eq!(induction_time_disc_pct(4), 10);
    assert_eq!(induction_time_disc_pct(5), 15);
    assert_eq!(induction_time_disc_pct(6), 15);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let state = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    assert_eq!(skill_discounts_and_cap(&state, None), (0, 0, 1));
    assert_eq!(max_runs_per_job(&state, 90_000_001), 1);

    let cid = 90_000_001i64;
    db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(cid).or_default();
        sk.insert(
            81377,
            TrainedSkillRecord {
                type_id: 81377,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        sk.insert(
            81366,
            TrainedSkillRecord {
                type_id: 81366,
                level: 3,
                skill_points: 50_000.0,
            },
        );
        sk.insert(
            81375,
            TrainedSkillRecord {
                type_id: 81375,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let (price_disc, dur_disc, max_jobs) = skill_discounts_and_cap(&state, Some(cid as u32));
    assert_eq!(price_disc, 0);
    assert!(dur_disc >= 10);
    assert_eq!(max_jobs, 6);
    assert_eq!(max_runs_per_job(&state, cid as u32), 50);

    let u = job_seq_to_uuid(9);
    assert_eq!(&u[0..8], &0x534B_494E_524A_4F42u64.to_be_bytes());
    assert_eq!(&u[8..16], &9u64.to_be_bytes());
    assert_eq!(hex_bytes(&[0xab, 0x05]), "ab05");
}

#[test]
fn thirdparty_skin_licenses_grant_activate_apply_unapply_and_revoke() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let ship_id = db.with_snapshot(|s| s.characters[&cid].active_ship_id) as u64;
    let ship_tid = db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);
    // Ensure default ship type in test matches pilot's active ship if needed
    db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&(ship_id as i64)) {
            it.type_id = DEFAULT_SHIP_TYPE_ID;
        }
    });
    let _ = ship_tid;

    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);
    let other = sess(Some(2), Some(90_000_002), service_roles::ROLE_PLAYER_DEFAULT);

    // 1. Admin Grant 3rd-party SKIN license ("skin_custom_01", qty=2)
    let mut grant_req = Vec::new();
    encode_bytes_field(1, &encode_skin_id("skin_custom_01"), &mut grant_req);
    encode_varint_field(2, 2, &mut grant_req);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
            grant_req,
            now
        )
        .status_code,
        STATUS_OK
    );

    // 2. Design GetRequest & License GetOwned / Get
    let mut design_req = Vec::new();
    encode_bytes_field(1, &encode_skin_id("skin_custom_01"), &mut design_req);
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest", design_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve.cosmetic.ship.skin.thirdparty.api.requests.GetRequest", design_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new(), now).status_code,
        STATUS_OK
    );

    let lic_id_bytes = encode_skin_license_id(cid as u32, "skin_custom_01");
    let mut lic_req = Vec::new();
    encode_bytes_field(1, &lic_id_bytes, &mut lic_req);
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &other, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );

    // 3. Apply before activation -> 400; Activate -> 200; Apply -> 200; Unapply -> 200; Revoke -> 200
    let mut ship_seq = Vec::new();
    encode_varint_field(1, ship_id, &mut ship_seq);
    let mut apply_req = lic_req.clone();
    encode_bytes_field(2, &ship_seq, &mut apply_req);
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest", apply_req.clone(), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest", lic_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest", apply_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest", apply_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &gm, "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest", lic_req, now).status_code,
        STATUS_OK
    );
}

#[test]
fn sequencing_pricing_durations_and_job_lifecycle() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.plex_balance = 5_000;
        }
    });
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);

    // Pricing, durations, early completion pricing, job capacity
    for ep in [
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
    ] {
        assert_eq!(call(&router, &player, ep, Vec::new(), now).status_code, STATUS_OK, "{ep}");
    }

    // Grant infinite coating component so InitiateRequest has required component license
    let st_tmp = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let coat_id = *st_tmp
        .catalog
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern() && !c.disallowed_slots.contains(&1))
        .map(|(id, _)| id)
        .expect("coating component");
    let mut inf_grant = Vec::new();
    encode_bytes_field(
        1,
        &super::super::quasar_cosmetics_skinr::encode_component_id(coat_id, false),
        &mut inf_grant,
    );
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            inf_grant,
            now
        )
        .status_code,
        STATUS_OK
    );

    // Initiate sequencing job for hull 587 (Rifter)
    let mut slot_entry = encode_layout_slot(&SkinLayoutSlot {
        slot_id: 1,
        component_id: coat_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    });
    let mut ship_seq = Vec::new();
    encode_varint_field(1, 587, &mut ship_seq);
    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_entry, &mut layout);
    encode_bytes_field(2, &ship_seq, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    slot_entry.clear();

    let mut init_req = Vec::new();
    encode_string_field(1, "Nebula Rifter", &mut init_req);
    encode_string_field(2, "Nebula Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
        now,
    );
    assert_eq!(init_rsp.status_code, STATUS_OK);

    // GetAllActiveRequest & CompleteEarlyRequest (empty body completes first active job)
    let act_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
        now,
    );
    assert_eq!(act_rsp.status_code, STATUS_OK);
    assert!(!act_rsp.payload.unwrap().value.is_empty());

    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
            Vec::new(),
            now + 10
        )
        .status_code,
        STATUS_OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_545_cluster_tick_settles_matured_skinr_job_and_publishes_notices() {
    let ctx = crate::router::ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let cid = 90_000_001_i64;
    let uid = 1_u64;
    let now = ctx.now_unix();
    let mut gm = sess(Some(uid), Some(cid), service_roles::ROLE_GM_DEFAULT);
    let mut player = sess(Some(uid), Some(cid), 0);
    gm.solar_system_id = Some(30_000_142);
    player.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.balance = eve_db::IskCents(1_000_000_000_00);
        }
        s.adjust_unified_plex(cid, 50_000);
        for sk in [82_478u32, 82_479, 82_480, 82_481, 82_482, 82_483] {
            s.skills.entry(cid).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let coat_id = SkinrStaticCatalog::load(&shared_sde())
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern() && !c.disallowed_slots.contains(&1))
        .map(|(&id, _)| id)
        .expect("coating component");
    let mut inf_grant = Vec::new();
    encode_bytes_field(
        1,
        &super::super::quasar_cosmetics_skinr::encode_component_id(coat_id, false),
        &mut inf_grant,
    );
    assert_eq!(
        call(
            &ctx.quasar_gateway,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            inf_grant,
            now
        )
        .status_code,
        STATUS_OK
    );

    let mut rx = ctx.quasar_gateway.subscribe_session(&player, Vec::new(), now);

    let mut slot_entry = encode_layout_slot(&SkinLayoutSlot {
        slot_id: 1,
        component_id: coat_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    });
    let mut ship_seq = Vec::new();
    encode_varint_field(1, 587, &mut ship_seq);
    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_entry, &mut layout);
    encode_bytes_field(2, &ship_seq, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    slot_entry.clear();

    let mut init_req = Vec::new();
    encode_string_field(1, "AutoSettle Rifter", &mut init_req);
    encode_string_field(2, "Tick Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = call(
        &ctx.quasar_gateway,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
        now,
    );
    assert_eq!(init_rsp.status_code, STATUS_OK);
    // Drain any notices generated during InitiateRequest before advancing clock
    while rx.try_recv().is_ok() {}

    // Advance past any job duration without calling GetAllActive/CompleteEarly/etc.
    ctx.advance_secs(7 * 86_400);
    crate::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let mut notice_urls = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            notice_urls.push(p.type_url);
        }
    }
    assert!(
        notice_urls.iter().any(|u| u.ends_with("sequencing.job.api.CompletedNotice")),
        "expected CompletedNotice emitted by background cluster tick, got {notice_urls:?}"
    );
    assert!(
        notice_urls.iter().any(|u| u.ends_with("thirdparty.license.api.AddedNotice")),
        "expected AddedNotice emitted by background cluster tick, got {notice_urls:?}"
    );

    // Calling GetAllActive afterward must see 0 active jobs (already settled)
    let act_rsp = call(
        &ctx.quasar_gateway,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
        ctx.now_unix(),
    );
    assert_eq!(act_rsp.status_code, STATUS_OK);
    assert!(act_rsp.payload.unwrap().value.is_empty());

    // Direct skill_discounts_and_cap & max_runs_per_job level progression tests
    let skinr_state = SkinrCosmeticsState::load_unregistered(ctx.db.clone(), ctx.sde.clone());
    let unseeded_cid = 99_999_888_u32;
    assert_eq!(skill_discounts_and_cap(&skinr_state, None), (0, 0, 1));
    assert_eq!(skill_discounts_and_cap(&skinr_state, Some(unseeded_cid)), (0, 0, 1));
    assert_eq!(max_runs_per_job(&skinr_state, unseeded_cid), 1);

    for (lvl, exp_par, exp_mt, exp_batch, exp_mass, exp_ind) in [
        (1u8, 1usize, 2usize, 4u32, 25u32, 200u32),
        (2, 3, 4, 9, 50, 450),
        (3, 5, 6, 19, 100, 700),
        (4, 7, 8, 29, 150, 1_700),
        (5, 9, 10, 49, 250, 4_700),
    ] {
        ctx.db.with_mut_snapshot(|s| {
            let base = s.characters.values().next().unwrap().clone();
            s.characters.entry(unseeded_cid as i64).or_insert(eve_db::CharacterRecord {
                character_id: unseeded_cid as i64,
                name: "Unseeded Skinr Seq".to_string(),
                ..base
            });
            let mut sm = std::collections::HashMap::new();
            for sid in [81366u32, 81367, 81375, 81364, 81365] {
                sm.insert(
                    sid,
                    eve_db::TrainedSkillRecord {
                        type_id: sid,
                        level: lvl,
                        skill_points: 1_000.0,
                    },
                );
            }
            s.skills.insert(unseeded_cid as i64, sm);
        });
        assert_eq!(
            skill_discounts_and_cap(&skinr_state, Some(unseeded_cid)).2,
            1 + exp_par + exp_mt
        );
        assert_eq!(
            max_runs_per_job(&skinr_state, unseeded_cid),
            1 + exp_batch + exp_mass + exp_ind
        );
    }
}

#[cfg(test)]
#[path = "quasar_cosmetics_skinr_seq_extra_unit_tests.rs"]
mod quasar_cosmetics_skinr_seq_extra_unit_tests;
