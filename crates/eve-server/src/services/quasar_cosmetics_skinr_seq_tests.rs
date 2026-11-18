//! Unit tests for `quasar_cosmetics_skinr_seq`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use crate::services::quasar_cosmetics_skinr::{build_layout_slot, SkinrStaticCatalog};
use eve_db::{TrainedSkillRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
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
    let grant_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
    )
    .set("skin", build_skin_id("eve_public", "skin_custom_01"))
    .set("unactivated", 2u64)
    .must_encode();
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
    let design_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest")
        .set("skin", build_skin_id("eve_public", "skin_custom_01"))
        .must_encode();
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

    let lic_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest")
        .set("license", build_skin_license_id("eve_public", cid as u32, "skin_custom_01"))
        .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &other, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", lic_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );

    // 3. Apply before activation -> 400; Activate -> 200; Apply -> 200; Unapply -> 200; Revoke -> 200
    let apply_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest")
        .set("license", build_skin_license_id("eve_public", cid as u32, "skin_custom_01"))
        .set("ship.sequential", ship_id)
        .must_encode();
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

    // Grant infinite coating component so InitiateRequest exercises sequence binder validation + consumption
    let st_tmp = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let (coat_id, coat_binder_tid, coat_binder_cnt) = st_tmp
        .catalog
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern() && !c.disallowed_slots.contains(&1) && c.binder_type_id > 0 && c.binder_count > 0)
        .map(|(&id, c)| (id, c.binder_type_id, c.binder_count))
        .expect("coating component with sequence binder");
    let inf_grant = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
    )
    .set(
        "component",
        super::super::quasar_cosmetics_skinr::build_component_id(coat_id, false),
    )
    .must_encode();
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
    let slot = SkinLayoutSlot {
        slot_id: 1,
        component_id: coat_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    };
    let init_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
    )
    .set("name", "Nebula Rifter")
    .set("line_name", "Nebula Line")
    .set_with("layout", |l| {
        l.add("slots", build_layout_slot("eve_public", &slot))
            .set("ship_type.sequential", 587u64)
            .set("pattern_blend_mode", 1i32)
    })
    .set("licenses", 1u32)
    .must_encode();

    // Missing sequence binders in hangar must reject with 400 and conserve PLEX & active jobs
    let plex_before_fail = db.with_snapshot(|s| s.unified_plex_balance(cid));
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req.clone(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(db.with_snapshot(|s| s.unified_plex_balance(cid)), plex_before_fail);
    assert!(st_tmp.data.read().unwrap().sequencing_jobs.is_empty());

    // Supply exact required sequence binders in hangar -> succeeds and consumes binders
    db.with_mut_snapshot(|s| {
        let station_id = s.characters.get(&cid).and_then(|c| c.station_id).unwrap_or(60_003_760);
        s.next_entity_id += 1;
        let iid = s.next_entity_id;
        s.items.insert(
            iid,
            eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: coat_binder_tid,
                owner_id: cid,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: coat_binder_cnt as i32,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let init_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
        now,
    );
    assert_eq!(init_rsp.status_code, STATUS_OK);
    assert_eq!(
        db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|it| it.owner_id == cid && it.type_id == coat_binder_tid)
                .map(|it| it.quantity)
                .sum::<i32>()
        }),
        0
    );

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

    let (coat_id, coat_binder_tid, coat_binder_cnt) = SkinrStaticCatalog::load(&shared_sde())
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern() && !c.disallowed_slots.contains(&1) && c.binder_type_id > 0 && c.binder_count > 0)
        .map(|(&id, c)| (id, c.binder_type_id, c.binder_count))
        .expect("coating component with sequence binder");
    ctx.db.with_mut_snapshot(|s| {
        let station_id = s.characters.get(&cid).and_then(|c| c.station_id).unwrap_or(60_003_760);
        s.next_entity_id += 1;
        let iid = s.next_entity_id;
        s.items.insert(
            iid,
            eve_db::InventoryItemRecord {
                item_id: iid,
                type_id: coat_binder_tid,
                owner_id: cid,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: coat_binder_cnt as i32,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let inf_grant = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
    )
    .set(
        "component",
        super::super::quasar_cosmetics_skinr::build_component_id(coat_id, false),
    )
    .must_encode();
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

    let slot = SkinLayoutSlot {
        slot_id: 1,
        component_id: coat_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    };
    let init_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
    )
    .set("name", "AutoSettle Rifter")
    .set("line_name", "Tick Line")
    .set_with("layout", |l| {
        l.add("slots", build_layout_slot("eve_public", &slot))
            .set("ship_type.sequential", 587u64)
            .set("pattern_blend_mode", 1i32)
    })
    .set("licenses", 1u32)
    .must_encode();

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

#[test]
fn sequencing_consume_binders_uses_consume_owner_items_and_emits_on_item_change() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), shared_sde());
    crate::services::quasar_cosmetics_skinr::register(&router, Arc::clone(&skinr));
    register(&router, Arc::clone(&skinr));

    let captured: Arc<std::sync::Mutex<Vec<(i64, String, Vec<eve_proto::PyRep>)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let cap = Arc::clone(&captured);
        *router.macho_notify_sink.write().unwrap() = Some(Arc::new(move |target, event, _id, args| {
            let crate::router::NotificationTarget::Character(c) = target else { return; };
            cap.lock().unwrap().push((c, event.to_string(), args));
        }));
    }

    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let slot1_allowed = skinr.catalog.slot_allowed_categories.get(&1);
    let coat = skinr
        .catalog
        .components
        .values()
        .find(|c| {
            !c.is_pattern()
                && !c.disallowed_slots.contains(&1)
                && slot1_allowed.is_none_or(|cats| cats.is_empty() || cats.contains(&c.category))
                && c.binder_type_id > 0
                && c.binder_count > 0
        })
        .expect("coating component with binder requirement");
    let btid = coat.binder_type_id;
    let bcnt = coat.binder_count as i32;

    db.with_mut_snapshot(|s| {
        s.adjust_unified_plex(cid, 5_000);
        let station_id = s.characters[&cid].station_id.unwrap_or(60_003_760);
        // Singleton binder stack must be rejected/skipped by ConsumeExactOptions::for_character_commodity.
        let bad_id = s.alloc_entity_id();
        s.items.insert(
            bad_id,
            eve_db::InventoryItemRecord {
                item_id: bad_id,
                type_id: btid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: bcnt * 5,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    skinr.data.write().unwrap().infinite_licenses.insert(
        SkinrCosmeticsState::comp_key(cid as u32, false, coat.component_id),
        crate::services::quasar_cosmetics_skinr::InfiniteComponentLicense {
            character_id: cid as u32,
            component_id: coat.component_id,
            is_pattern: false,
            first_acquired_unix: now,
            is_free: false,
        },
    );

    let slot = SkinLayoutSlot {
        slot_id: 1,
        component_id: coat.component_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    };
    let init_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest")
        .set("name", "Binder Invariant SKIN")
        .set("line_name", "Binder Line")
        .set_with("layout", |l| {
            l.add("slots", build_layout_slot("eve_public", &slot))
                .set("ship_type.sequential", 587u64)
                .set("pattern_blend_mode", 1i32)
        })
        .set("licenses", 1u32)
        .must_encode();

    // Only singleton stack present -> must fail cleanly without creating job or consuming PLEX
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req.clone(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // Seed two valid non-singleton binder stacks: one exact-consumed (deleted) + one partially decremented
    let (id1, id2) = db.with_mut_snapshot(|s| {
        let station_id = s.characters[&cid].station_id.unwrap_or(60_003_760);
        let first_qty = (bcnt - 1).max(1);
        let id1 = s.alloc_entity_id();
        s.items.insert(
            id1,
            eve_db::InventoryItemRecord {
                item_id: id1,
                type_id: btid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: first_qty,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let id2 = s.alloc_entity_id();
        s.items.insert(
            id2,
            eve_db::InventoryItemRecord {
                item_id: id2,
                type_id: btid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: bcnt + 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (id1, id2)
    });

    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        if bcnt > 1 {
            assert!(!s.items.contains_key(&id1), "depleted binder stack should be removed");
            assert_eq!(s.items[&id2].quantity, (bcnt + 5) - 1);
        }
    });
    let item_changes: Vec<_> = captured
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, ev, _)| ev == "OnItemChange")
        .cloned()
        .collect();
    assert!(!item_changes.is_empty(), "binder consumption must emit OnItemChange");
    for (_, _, args) in &item_changes {
        if let eve_proto::PyRep::Dict(entries) = &args[1] {
            assert!(
                entries.iter().any(|(k, _)| matches!(k, eve_proto::PyRep::Integer(9))),
                "OnItemChange must use ixStackSize=9, got {entries:?}"
            );
            assert!(
                !entries.iter().any(|(k, _)| matches!(k, eve_proto::PyRep::Integer(5))),
                "OnItemChange must never emit ixQuantity=5"
            );
        }
    }
}

#[test]
fn sequencing_binder_location_blocks_remote_hangar_and_allows_in_space_cargo() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), shared_sde());
    crate::services::quasar_cosmetics_skinr::register(&router, Arc::clone(&skinr));
    register(&router, Arc::clone(&skinr));

    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let remote_station = 60_003_761i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let slot1_allowed = skinr.catalog.slot_allowed_categories.get(&1);
    let coat = skinr
        .catalog
        .components
        .values()
        .find(|c| {
            !c.is_pattern()
                && !c.disallowed_slots.contains(&1)
                && slot1_allowed.is_none_or(|cats| cats.is_empty() || cats.contains(&c.category))
                && c.binder_type_id > 0
                && c.binder_count > 0
        })
        .expect("coating component with binder requirement");
    let btid = coat.binder_type_id;
    let bcnt = coat.binder_count as i32;

    let (home_station, active_ship_id, plex_before) = db.with_mut_snapshot(|s| {
        s.adjust_unified_plex(cid, 5_000);
        let ch = &s.characters[&cid];
        let home_station = ch.station_id.unwrap_or(60_003_760);
        let active_ship_id = ch.active_ship_id;
        assert_ne!(home_station, remote_station);
        // Seed binders ONLY at a remote station hangar.
        let remote_id = s.alloc_entity_id();
        s.items.insert(
            remote_id,
            eve_db::InventoryItemRecord {
                item_id: remote_id,
                type_id: btid,
                owner_id: cid,
                location_id: remote_station,
                flag: InventoryFlag::Hangar,
                quantity: bcnt * 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (home_station, active_ship_id, s.unified_plex_balance(cid))
    });
    skinr.data.write().unwrap().infinite_licenses.insert(
        SkinrCosmeticsState::comp_key(cid as u32, false, coat.component_id),
        crate::services::quasar_cosmetics_skinr::InfiniteComponentLicense {
            character_id: cid as u32,
            component_id: coat.component_id,
            is_pattern: false,
            first_acquired_unix: now,
            is_free: false,
        },
    );
    let slot = SkinLayoutSlot {
        slot_id: 1,
        component_id: coat.component_id,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    };
    let init_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest")
        .set("name", "Location Guard SKIN")
        .set("line_name", "Loc Line")
        .set_with("layout", |l| {
            l.add("slots", build_layout_slot("eve_public", &slot))
                .set("ship_type.sequential", 587u64)
                .set("pattern_blend_mode", 1i32)
        })
        .set("licenses", 1u32)
        .must_encode();

    // 1. Docked at home_station: remote-station binders MUST NOT be consumed (400 + PLEX unchanged).
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req.clone(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    db.with_snapshot(|s| {
        assert_eq!(s.unified_plex_balance(cid), plex_before);
    });

    // 2. Undocked in space: home-station hangar binders remain ignored; active-ship Cargo binders work.
    let cargo_id = db.with_mut_snapshot(|s| {
        let sys_id = s.characters[&cid].solar_system_id;
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        if let Some(ship) = s.items.get_mut(&active_ship_id) {
            ship.location_id = i64::from(sys_id);
        }
        let home_hangar_id = s.alloc_entity_id();
        s.items.insert(
            home_hangar_id,
            eve_db::InventoryItemRecord {
                item_id: home_hangar_id,
                type_id: btid,
                owner_id: cid,
                location_id: home_station,
                flag: InventoryFlag::Hangar,
                quantity: bcnt * 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let cargo_id = s.alloc_entity_id();
        s.items.insert(
            cargo_id,
            eve_db::InventoryItemRecord {
                item_id: cargo_id,
                type_id: btid,
                owner_id: cid,
                location_id: active_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: bcnt + 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        cargo_id
    });

    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        assert_eq!(s.items[&cargo_id].quantity, 3, "in-space Cargo binders must be consumed");
        assert!(s.unified_plex_balance(cid) < plex_before);
    });
}

#[test]
fn initiate_rejects_invalid_and_disallowed_pattern_nested_coatings() {
    use crate::services::quasar_cosmetics_skinr::{
        SdeSkinComponent, COMP_CAT_MATERIAL, COMP_CAT_PATTERN,
    };

    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let base_skinr = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), shared_sde());
    let mut catalog = base_skinr.catalog.clone();

    let pat_id = 990_001u32;
    let bad_pat_as_coat_id = 990_002u32;
    let ok_coat_id = 990_003u32;
    let disallowed_slot6_coat_id = 990_004u32;
    let disallowed_slot8_coat_id = 990_005u32;
    let bad_cat_coat_id = 990_006u32;

    for (component_id, category, disallowed_slots) in [
        (pat_id, COMP_CAT_PATTERN, vec![]),
        (bad_pat_as_coat_id, COMP_CAT_PATTERN, vec![]),
        (ok_coat_id, COMP_CAT_MATERIAL, vec![]),
        (disallowed_slot6_coat_id, COMP_CAT_MATERIAL, vec![6]),
        (disallowed_slot8_coat_id, COMP_CAT_MATERIAL, vec![8]),
        (bad_cat_coat_id, 99, vec![]),
    ] {
        catalog.components.insert(
            component_id,
            SdeSkinComponent {
                component_id,
                category,
                rarity: 1,
                disallowed_slots,
                associated_types: Vec::new(),
                binder_type_id: 0,
                binder_count: 0,
            },
        );
    }
    catalog
        .slot_allowed_categories
        .entry(6)
        .or_insert_with(|| vec![COMP_CAT_MATERIAL]);
    catalog
        .slot_allowed_categories
        .entry(8)
        .or_insert_with(|| vec![COMP_CAT_MATERIAL]);

    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let mut data = base_skinr.data.read().unwrap().clone();
    for (comp_id, is_pattern) in [
        (pat_id, true),
        (bad_pat_as_coat_id, true),
        (ok_coat_id, false),
        (disallowed_slot6_coat_id, false),
        (disallowed_slot8_coat_id, false),
        (bad_cat_coat_id, false),
    ] {
        data.infinite_licenses.insert(
            SkinrCosmeticsState::comp_key(cid as u32, is_pattern, comp_id),
            crate::services::quasar_cosmetics_skinr::InfiniteComponentLicense {
                character_id: cid as u32,
                component_id: comp_id,
                is_pattern,
                first_acquired_unix: now,
                is_free: true,
            },
        );
    }
    db.with_mut_snapshot(|s| {
        s.adjust_unified_plex(cid, 10_000);
    });

    let skinr = Arc::new(SkinrCosmeticsState {
        db: Arc::clone(&db),
        sde: shared_sde(),
        catalog,
        data: std::sync::RwLock::new(data),
        last_redeemed_gen: std::sync::atomic::AtomicU64::new(0),
    });
    crate::services::quasar_cosmetics_skinr::register(&router, Arc::clone(&skinr));
    register(&router, Arc::clone(&skinr));

    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let build_v24_pat_init = |slot6_comp: u32, slot8_comp: u32| {
        let slot5 = SkinLayoutSlot {
            slot_id: 5,
            component_id: pat_id,
            is_pattern: true,
            primary_coating_id: None,
            secondary_coating_id: None,
        };
        let slot6 = SkinLayoutSlot {
            slot_id: 6,
            component_id: slot6_comp,
            is_pattern: false,
            primary_coating_id: None,
            secondary_coating_id: None,
        };
        let slot8 = SkinLayoutSlot {
            slot_id: 8,
            component_id: slot8_comp,
            is_pattern: false,
            primary_coating_id: None,
            secondary_coating_id: None,
        };
        pb::message("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest")
            .set("name", "Nested Coating Validation")
            .set("line_name", "Validation Line")
            .set_with("layout", |l| {
                l.add("slots", build_layout_slot("eve_public", &slot5))
                    .add("slots", build_layout_slot("eve_public", &slot6))
                    .add("slots", build_layout_slot("eve_public", &slot8))
                    .set("ship_type.sequential", 587u64)
                    .set("pattern_blend_mode", 1i32)
            })
            .set("licenses", 1u32)
            .must_encode()
    };
    let build_nested_pat_layout_msg = |p_coat: Option<u32>, s_coat: Option<u32>| {
        use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};
        let seq_msg = |id: u32| {
            let mut m = DynamicQuasarMessage::new("");
            m.set(1, DynamicValue::Uint64(u64::from(id)));
            DynamicValue::Message(Box::new(m))
        };
        let mut pat_cfg = DynamicQuasarMessage::new("");
        pat_cfg.set(1, seq_msg(pat_id));
        if let Some(pc) = p_coat {
            pat_cfg.set(2, seq_msg(pc));
        }
        if let Some(sc) = s_coat {
            pat_cfg.set(3, seq_msg(sc));
        }
        let mut cfg = DynamicQuasarMessage::new("");
        cfg.set(2, DynamicValue::Message(Box::new(pat_cfg)));
        let mut slot_msg = DynamicQuasarMessage::new("");
        slot_msg.set(1, seq_msg(5));
        slot_msg.set(2, DynamicValue::Message(Box::new(cfg)));
        let mut layout_msg = DynamicQuasarMessage::new("");
        layout_msg.set(1, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(slot_msg))]));
        layout_msg
    };

    for (label, p_coat, s_coat) in [
        ("pattern ID in primary_coating_id (slot 6)", bad_pat_as_coat_id, ok_coat_id),
        ("pattern ID in secondary_coating_id (slot 8)", ok_coat_id, bad_pat_as_coat_id),
        ("disallowed_slots=[6] in primary_coating_id", disallowed_slot6_coat_id, ok_coat_id),
        ("disallowed_slots=[8] in secondary_coating_id", ok_coat_id, disallowed_slot8_coat_id),
        ("disallowed category 99 in primary_coating_id", bad_cat_coat_id, ok_coat_id),
        ("disallowed category 99 in secondary_coating_id", ok_coat_id, bad_cat_coat_id),
    ] {
        // 1) Direct parse_initiate_slots nested pattern-coating checks (`pat_cfg` fields 2/3)
        let nested_res = parse_initiate_slots(
            &skinr,
            &build_nested_pat_layout_msg(Some(p_coat), Some(s_coat)),
            587,
        );
        assert_eq!(
            nested_res.unwrap_err().0,
            STATUS_BAD_REQUEST,
            "expected 400 from parse_initiate_slots nested branch: {label}"
        );

        // 2) Full InitiateRequest wire dispatch with V24 pattern material slots 6 and 8
        let rsp = call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            build_v24_pat_init(p_coat, s_coat),
            now,
        );
        assert_eq!(
            rsp.status_code, STATUS_BAD_REQUEST,
            "expected 400 from InitiateRequest V24 slot 6/8 branch: {label}"
        );
    }

    let (parsed_slots, parsed_reqs) = parse_initiate_slots(
        &skinr,
        &build_nested_pat_layout_msg(Some(ok_coat_id), Some(ok_coat_id)),
        587,
    )
    .expect("valid nested coatings should parse");
    assert_eq!(parsed_slots[0].primary_coating_id, Some(ok_coat_id));
    assert_eq!(parsed_slots[0].secondary_coating_id, Some(ok_coat_id));
    assert!(parsed_reqs.contains(&(ok_coat_id, false)));

    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            build_v24_pat_init(ok_coat_id, ok_coat_id),
            now,
        )
        .status_code,
        STATUS_OK
    );
}

#[test]
fn binder_plan_partial_failure_rolls_back_plex_and_prior_binder_stacks() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let cid = 90_000_001i64;
    let btid_1 = 83_000u32;
    let btid_2 = 83_001u32;

    let (station_id, stack_1_id, stack_2_id, plex_before, next_id_before) =
        db.with_mut_snapshot(|s| {
            s.adjust_unified_plex(cid, 5_000);
            let station_id = s.characters[&cid].station_id.unwrap_or(60_003_760);
            let id1 = s.alloc_entity_id();
            s.items.insert(
                id1,
                eve_db::InventoryItemRecord {
                    item_id: id1,
                    type_id: btid_1,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            let id2 = s.alloc_entity_id();
            s.items.insert(
                id2,
                eve_db::InventoryItemRecord {
                    item_id: id2,
                    type_id: btid_2,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            (
                station_id,
                id1,
                id2,
                s.unified_plex_balance(cid),
                s.next_entity_id,
            )
        });

    let mut hangar_opts = eve_db::ConsumeExactOptions::for_character_commodity(cid);
    hangar_opts.required_location_id = Some(station_id);
    hangar_opts.required_flag = Some(InventoryFlag::Hangar);

    let err = db
        .try_with_mut_snapshot(|s| {
            debit_sequencing_binders_and_plex_in_snap(
                s,
                cid as u32,
                300,
                &[(btid_1, hangar_opts, 4), (btid_2, hangar_opts, 5)],
            )
        })
        .unwrap_err();
    assert!(err.contains("Insufficient sequence binders"), "unexpected err: {err}");

    db.with_snapshot(|s| {
        assert_eq!(
            s.unified_plex_balance(cid),
            plex_before,
            "failed 2nd binder consumption must roll back PLEX debit atomically"
        );
        assert_eq!(
            s.items[&stack_1_id].quantity, 10,
            "failed 2nd binder consumption must roll back 1st binder stack consumption atomically"
        );
        assert_eq!(s.items[&stack_2_id].quantity, 1);
        assert_eq!(s.next_entity_id, next_id_before);
    });
}

#[cfg(test)]
#[path = "quasar_cosmetics_skinr_seq_extra_unit_tests.rs"]
mod quasar_cosmetics_skinr_seq_extra_unit_tests;
