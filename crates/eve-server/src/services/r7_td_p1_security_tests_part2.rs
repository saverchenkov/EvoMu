//! Security and mechanics regression tests for Round-7 Track D Priority-1 fixes (`#464`, `#465`).

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::campaigns_and_milestones::{
    self, SdeLoginRewardEntry, deliver_login_reward, get_sde_login_campaigns,
};
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};
use crate::services::social_corp_fleet::social_corp_projects::{
    CanonicalProjectSpec, create_canonical_project, effective_corp_member_roles, freelance_uuid,
};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, corp_roles,
};
use eve_proto::PyRep;
use eve_proto::quasar::{
    EvePublicRequest, GoogleAny, TYPE_GOOGLEAPIS_PREFIX, WireMessage, encode_bytes_field,
    encode_varint_field,
};
use std::collections::HashMap;

/// `#464`: Masks all unmatured 24h title role bits (`ROLE_DIRECTOR`, `ROLE_PROJECT_MANAGER`,
/// `ROLE_ACCOUNT_CAN_TAKE`) in `effective_corp_member_roles` and rejects assembled singleton ships/containers
/// or items containing nested child items during project item delivery.
#[test]
fn r7_464_title_role_maturation_and_singleton_ship_container_delivery_blocked() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_002_i64;
    let member_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;

    let privileged_title_roles = corp_roles::ROLE_DIRECTOR
        | corp_roles::ROLE_PROJECT_MANAGER
        | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Title Maturation Corp".into(),
                ceo_id,
                master_wallet: IskCents::from_isk(20_000_000),
                wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents::from_isk(
                    20_000_000,
                )),
                ..Default::default()
            },
        );
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.titles
            .insert(1_i64, ("Executive".to_string(), privileged_title_roles));
        gov.member_titles.insert(member_id, 1_u64);
        gov.pending_title_role_grants
            .insert(member_id, (privileged_title_roles, now + 86_400));
    });

    // Before 24h maturation completes, none of the unmatured title bits may be active.
    let (_, pre_mature_roles) =
        ctx.db.with_snapshot(|s| effective_corp_member_roles(s, corp_id, member_id, 0, now));
    assert_eq!(pre_mature_roles & corp_roles::ROLE_DIRECTOR, 0);
    assert_eq!(pre_mature_roles & corp_roles::ROLE_PROJECT_MANAGER, 0);
    assert_eq!(pre_mature_roles & corp_roles::ROLE_ACCOUNT_CAN_TAKE[0], 0);

    // Once 24h maturation elapses, all granted title bits become active.
    let (_, matured_roles) = ctx
        .db
        .with_snapshot(|s| effective_corp_member_roles(s, corp_id, member_id, 0, now + 86_400));
    assert_ne!(matured_roles & corp_roles::ROLE_DIRECTOR, 0);
    assert_ne!(matured_roles & corp_roles::ROLE_PROJECT_MANAGER, 0);
    assert_ne!(matured_roles & corp_roles::ROLE_ACCOUNT_CAN_TAKE[0], 0);

    // Part 2: DeliverRequest must reject assembled singleton ships or containers with nested child items.
    let ship_tid = 587_u32;
    let (fl_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: ceo_id,
            name: "Repackaged Hull Contract".into(),
            description: "Reject assembled singletons".into(),
            target_quantity: 5,
            initial_quantity: 0,
            escrow: IskCents::from_isk(5_000_000),
            isk_per_unit: IskCents::from_isk(1_000_000),
            wallet_division: 1,
            matcher_type: "DeliverItem".into(),
            is_freelance: true,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 3_600),
            filter_id: Some(i64::from(ship_tid)),
            batch_size: 1,
            now_unix: now,
            min_project_id: 500,
        },
    )
    .unwrap();

    let (assembled_ship_id, fitted_module_id) = (960_001_i64, 960_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&fl_pid) {
            det.committed_participants.push(member_id);
        }
        snap.items.insert(
            assembled_ship_id,
            InventoryItemRecord {
                item_id: assembled_ship_id,
                type_id: ship_tid,
                owner_id: member_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            fitted_module_id,
            InventoryItemRecord {
                item_id: fitted_module_id,
                type_id: 2456,
                owner_id: member_id,
                location_id: assembled_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let fl_uuid = freelance_uuid(now, fl_pid);
    let mut goal_id_bytes = Vec::new();
    encode_bytes_field(1, &fl_uuid, &mut goal_id_bytes);
    let mut item_id_bytes = Vec::new();
    encode_varint_field(1, assembled_ship_id as u64, &mut item_id_bytes);

    let mut deliver_bytes = Vec::new();
    encode_bytes_field(1, &goal_id_bytes, &mut deliver_bytes);
    encode_bytes_field(3, &item_id_bytes, &mut deliver_bytes);
    encode_varint_field(4, 1, &mut deliver_bytes);

    let mut sess = auth_session(1, member_id, corp_id, 30_000_142);
    sess.station_id = Some(station_id);
    let deliver_req = EvePublicRequest {
        correlation_uuid: vec![15; 16],
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest"
            ),
            value: deliver_bytes,
        }),
        ..Default::default()
    };
    let resp = ctx
        .quasar_gateway
        .handle_public_request(&sess, &deliver_req, None, false, now);
    assert_eq!(resp.status_code, 200);
    let decoded = WireMessage::decode(&resp.payload.unwrap().value).unwrap();
    assert!(
        decoded.fields.iter().any(|f| f.field_number == 9),
        "assembled singleton ship with fitted child items must be rejected with invalid_item_type (field 9)"
    );
    ctx.db.with_snapshot(|s| {
        assert!(s.items.contains_key(&assembled_ship_id));
        assert!(s.items.contains_key(&fitted_module_id));
        assert_eq!(s.corp_projects[&fl_pid].current_quantity, 0);
    });
}

/// `#465`: Docked login rewards spawn packaged singletons (`singleton: false` when not a BPC),
/// preserve `expireDateTime`, emit `OnItemChange`, and scope `login_campaign_claimed_days` per `(char_id, campaign_id)`.
#[tokio::test]
async fn r7_465_login_reward_packaging_expiry_item_change_and_campaign_scoped_claims() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    let kw = HashMap::new();

    let mut camp_ids: Vec<i64> = get_sde_login_campaigns(&ctx.sde).keys().copied().collect();
    camp_ids.sort_unstable();
    assert!(
        camp_ids.len() >= 2,
        "SDE must define at least 2 login campaigns to verify campaign rotation isolation"
    );
    let (camp_a, camp_b) = (camp_ids[0], camp_ids[1]);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
            ch.clone_grade = 1;
        }
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_login_campaign_id = Some(camp_a);
        ext.login_campaign_claimed_days.clear();
        save_cosmetics_ui_state(snap, &ext);
    });

    let rep_a = campaigns_and_milestones::handle(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "claim_reward",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts_a) = rep_a else {
        panic!("expected tuple from claim_reward");
    };
    let item_id_a = parts_a[0].dict_get("itemID").and_then(|v| v.as_i64()).unwrap();

    let expected_singleton = get_sde_login_campaigns(&ctx.sde)[&camp_a]
        .alpha_rewards
        .get(&0)
        .is_some_and(|rw| rw.blueprint_runs.is_some());
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&item_id_a).expect("delivered item must exist in station hangar");
        assert_eq!(it.location_id, station_id);
        assert_eq!(it.singleton, expected_singleton);
        let ext = load_cosmetics_ui_state(snap);
        assert!(ext.login_campaign_claimed_days.contains_key(&(cid, camp_a)));
        assert!(!ext.login_campaign_claimed_days.contains_key(&(cid, camp_b)));
    });

    let sess_notifs = sess.drain_notifications();
    let char_notifs = ctx.drain_notifications_for_char(cid);
    assert!(
        sess_notifs.iter().any(|(ev, _)| ev == "OnItemChange")
            || char_notifs.iter().any(|n| n.event == "OnItemChange"),
        "station hangar login reward delivery must emit OnItemChange"
    );

    // Rotate active login campaign to campaign B: claim history must be scoped by `(char_id, campaign_id)`.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_login_campaign_id = Some(camp_b);
        save_cosmetics_ui_state(snap, &ext);
    });
    let state_b = campaigns_and_milestones::handle(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(state_b.dict_get("total_days_claimed"), Some(&PyRep::Integer(0)));
    assert_eq!(state_b.dict_get("can_claim_today"), Some(&PyRep::Bool(true)));

    let rep_b = campaigns_and_milestones::handle(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "claim_reward",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts_b) = rep_b else {
        panic!("expected tuple from campaign B claim_reward");
    };
    assert_eq!(
        parts_b[0].dict_get("reward_index"),
        Some(&PyRep::Integer(1)),
        "new campaign must start at day index 1 after rotating active_login_campaign_id"
    );

    let expiring_rw = SdeLoginRewardEntry {
        type_id: 34,
        quantity: 1,
        tier: 1,
        blueprint_runs: None,
        blueprint_material_level: None,
        blueprint_productivity_level: None,
        redeemed_expiry_days: Some(30),
        is_auto_injected_booster_or_skin: false,
        overlay_texture_path: None,
    };
    let now_u = ctx.now_unix();
    let expected_ft = ((now_u.saturating_add(30 * 86_400) as i64) + 11_644_473_600) * 10_000_000;
    let del_id = ctx.db.with_mut_snapshot(|snap| {
        deliver_login_reward(
            snap,
            uid,
            cid,
            Some(station_id),
            camp_a,
            now_u,
            &expiring_rw,
        )
        .primary_id
    });
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&del_id];
        assert!(!it.singleton, "1-unit stackable login reward must spawn packaged (singleton=false)");
        assert_eq!(
            snap.service_state_blobs
                .get(&format!("login_reward_expiry:{del_id}"))
                .and_then(|s| s.parse::<i64>().ok()),
            Some(expected_ft),
            "docked expiring login reward must preserve expireDateTime"
        );
    });
}
