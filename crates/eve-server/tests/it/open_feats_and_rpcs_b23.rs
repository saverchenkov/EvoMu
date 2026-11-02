//! Integration tests closing 17 `feat:cosmetics.*` and `feat:crimewatch.*` parity rows (Batch 23).
use crate::plan_c7_crimewatch_closure::{dict_get, item_rec, setup_pilot_in_space};
use crate::quasar_goal_tale_notices::{bytes, sub, varint};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_proto::PyRep;
use eve_server::combat::crimewatch::AggressionEvaluation;
use eve_server::combat::npc_ai::compute_sec_status_gain;
use eve_server::formulas::concord_response_delay_sec;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    check_crimewatch_action_restriction, evaluate_and_record_pvp_aggression,
    evaluate_and_record_remote_assistance, evaluate_faction_police_response,
    faction_police_sec_threshold, with_mut_combat_runtime, SafetyLevel,
};
use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;
use glam::DVec3;
use std::collections::HashMap;
const JITA_SYS: u32 = 30_000_142; // 0.9
const AMARR_SYS: u32 = 30_002_187; // 1.0
const UEDAMA_SYS: u32 = 30_002_768; // 0.5
const TAMA_SYS: u32 = 30_002_813; // 0.3 (low-sec)
const HED_GP_SYS: u32 = 30_001_161; // -0.2 (null-sec)
fn player_sess(sid: u64, cid: u32, corp: u32) -> SessionState {
    let mut s = build_sess(sid, cid, corp);
    s.user_id = Some(sid);
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s
}

fn seed_market_char(ctx: &ServerContext, cid: i64, corp: u32, isk: i64, plex: i64) {
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        let ch = s.characters.entry(cid).or_insert_with(|| template);
        ch.character_id = cid;
        ch.corporation_id = corp as i64;
        ch.balance = IskCents::from_isk(isk);
        ch.plex_balance = plex;
        ch.loyalty_points.insert(PARAGON_CORP_ID as i64, 100_000);
        ch.station_id = Some(60_003_760);
    });
}

#[tokio::test]
async fn test_b23_skinr_sequencing_and_paragon_hub_listing() {
    let ctx = ServerContext::new();
    let seller_cid: u32 = 95_230_001;
    let buyer_cid: u32 = 95_230_002;
    let corp_id: u32 = 98_230_001;
    seed_market_char(&ctx, seller_cid as i64, corp_id, 50_000_000, 5_000);
    seed_market_char(&ctx, buyer_cid as i64, corp_id, 50_000_000, 5_000);
    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(seller_cid as i64).or_default();
        for &(tid, lvl) in &[
            (81377, 5),
            (81372, 5),
            (84217, 5),
            (84218, 5),
            (81368, 2),
        ] {
            sk.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let mut seller = player_sess(952_301, seller_cid, corp_id);
    let mut seller_gm = build_sess(952_309, seller_cid, corp_id);
    let mut buyer = player_sess(952_302, buyer_cid, corp_id);
    // 1. Check tier durations scaled by SDE Induction skills (30% discount)
    let rsp_dur = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        Vec::new(),
    ).await;
    assert_eq!(rsp_dur.status_code, STATUS_OK);
    let wm_dur = WireMessage::decode(&rsp_dur.payload.as_ref().unwrap().value).expect("decode dur");
    assert_eq!(varint(&wm_dur, 2), Some(30));
    // 2. Check Paragon Hub tax rate reduced by Paragon Relations + Paragon Partnership (20% - 10% = 10%)
    let rsp_tax = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        Vec::new(),
    ).await;
    assert_eq!(rsp_tax.status_code, STATUS_OK);
    let wm_tax = WireMessage::decode(&rsp_tax.payload.as_ref().unwrap().value).expect("decode tax");
    let tr_msg = sub(&wm_tax, 1);
    assert_eq!(varint(&tr_msg, 1), Some(20)); // sint64(10) zigzag-encoded = 20
    // 3. Grant unactivated custom SKIN license, list on Paragon Hub, and have buyer purchase it
    let skin_hex = "b2300001000000000000000000000001";
    let mut grant_req = Vec::new();
    let mut skin_id = Vec::new();
    encode_string_field(1, skin_hex, &mut skin_id);
    encode_bytes_field(1, &skin_id, &mut grant_req);
    encode_varint_field(2, 2, &mut grant_req);
    let rsp_grant = quasar_rpc(
        &ctx,
        &mut seller_gm,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        grant_req,
    ).await;
    assert_eq!(rsp_grant.status_code, STATUS_OK);
    let mut price_isk = Vec::new();
    encode_varint_field(1, 1_000_000 * 2, &mut price_isk); // sint64 zigzag of 1_000_000
    let mut lic_id_msg = Vec::new();
    let mut seller_seq = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seller_seq);
    encode_bytes_field(1, &seller_seq, &mut lic_id_msg);
    encode_bytes_field(2, &skin_id, &mut lic_id_msg);
    let mut dur_msg = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg);
    let mut target_msg = Vec::new();
    encode_varint_field(1, 1, &mut target_msg);
    let mut create_req = Vec::new();
    encode_bytes_field(1, &price_isk, &mut create_req);
    encode_bytes_field(3, &lic_id_msg, &mut create_req);
    encode_bytes_field(4, &dur_msg, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);
    encode_bytes_field(6, &target_msg, &mut create_req);
    let rsp_create = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_req,
    ).await;
    assert_eq!(rsp_create.status_code, STATUS_OK);
    let rsp_owned = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
    ).await;
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    let wm_owned = WireMessage::decode(&rsp_owned.payload.as_ref().unwrap().value).expect("decode owned");
    let owned_listing = sub(&wm_owned, 1);
    let listing_uuid = bytes(&sub(&owned_listing, 1), 1);
    let listing_price_isk = sub(&sub(&owned_listing, 2), 5);
    assert_eq!(varint(&listing_price_isk, 1), Some(1_000_000 * 2)); // sint64 ZigZag-encoded
    let seller_bal_before_sale = ctx
        .db
        .with_snapshot(|s| s.characters[&(seller_cid as i64)].balance.0);
    let mut uuid_msg = Vec::new();
    encode_bytes_field(1, &listing_uuid, &mut uuid_msg);
    let mut buy_req = Vec::new();
    encode_bytes_field(1, &uuid_msg, &mut buy_req);
    encode_varint_field(2, 1, &mut buy_req);
    let rsp_buy = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    ).await;
    assert_eq!(rsp_buy.status_code, STATUS_OK);
    let seller_bal_after_sale = ctx
        .db
        .with_snapshot(|s| s.characters[&(seller_cid as i64)].balance.0);
    assert_eq!(seller_bal_after_sale - seller_bal_before_sale, 90_000_000);
}

#[tokio::test]
async fn test_b23_sec_status_caps_tags_outlaw_and_faction_police() {
    let ctx = ServerContext::new();
    let char_id: i64 = 95_230_010;
    let ship_id: i64 = 105_230_010;
    let pod_id: i64 = 105_230_011;
    let lowsec_sec_office_station = ctx
        .sde
        .stations
        .values()
        .find_map(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
            let has_office = ctx
                .sde
                .station_operations
                .get(&st.operation_id)
                .is_some_and(|op| {
                    op.services.iter().any(|sid| {
                        ctx.sde.station_services.get(sid).is_some_and(|svc| {
                            svc.name.eq_ignore_ascii_case("Security Office")
                                || svc.name.eq_ignore_ascii_case("securityOffice")
                        })
                    })
                })
                || st.owner_corporation_id == 1_000_125
                || st.owner_corporation_id == 1_000_137;
            (0.0 < sec && sec < 0.45 && has_office).then_some(st.station_id)
        })
        .unwrap_or(60_014_861);
    let mut sess = setup_pilot_in_space(&ctx, 952_310, char_id, 98_230_002, ship_id, 621, JITA_SYS, DVec3::ZERO).await;
    sess.station_id = Some(lowsec_sec_office_station);
    // 1. Security status gain caps at +5.0 from NPC kills
    assert_eq!(compute_sec_status_gain(5.0, 1_000_000.0), 0.0);
    let near_cap = compute_sec_status_gain(4.9999, 1_000_000.0);
    assert!((4.9999 + near_cap) <= 5.0 + 1e-9);
    // 2. Clone soldier tag tier bands: Trainer (-10..-8), Recruiter (-8..-5), Transporter (-5..-2), Negotiator (-2..0)
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_sec_office_station);
        ch.balance = IskCents::from_isk(50_000_000);
        ch.security_status = -9.0;
        s.items.insert(
            700_001,
            item_rec(700_001, 35661, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
        );
        s.items.insert(
            700_002,
            item_rec(700_002, 35662, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
        );
        s.items.insert(
            700_003,
            item_rec(700_003, 35664, char_id, lowsec_sec_office_station, InventoryFlag::Hangar, 2, false),
        );
    });
    let kw = HashMap::new();
    let bad_tag = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(35662), PyRep::Integer(1)],
            &kw,
            None,
        ).await;
    assert_eq!(bad_tag.unwrap_err().code, "SecurityTagBandMismatch");
    let ok_tag = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(35661), PyRep::Integer(1)],
            &kw,
            None,
        ).await
        .expect("Trainer tag accepted");
    assert_eq!(
        dict_get(&ok_tag, "newSecurityStatus").and_then(|v| v.as_f64()),
        Some(-8.5)
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().security_status = -0.2;
    });
    let cap_tag = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(35664), PyRep::Integer(1)],
            &kw,
            None,
        ).await
        .expect("Negotiator tag crosses 0.0 to +0.3");
    assert!(
        (dict_get(&cap_tag, "newSecurityStatus")
            .and_then(|v| v.as_f64())
            .unwrap()
            - 0.3)
            .abs()
            < 1e-6
    );
    // 3. Faction police thresholds per system security & pod vs ship
    assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
    assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
    assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
    assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
    assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().security_status = -5.0;
        s.items.insert(
            pod_id,
            item_rec(pod_id, 670, char_id, JITA_SYS as i64, InventoryFlag::None, 1, true),
        );
    });
    let (engage_ship, allow_ship_dock, _) =
        evaluate_faction_police_response(&ctx, JITA_SYS, char_id, ship_id).await;
    assert!(engage_ship);
    assert!(!allow_ship_dock);
    let (engage_pod, allow_pod_dock, _) =
        evaluate_faction_police_response(&ctx, JITA_SYS, char_id, pod_id).await;
    assert!(!engage_pod);
    assert!(allow_pod_dock);
    // 4. Outlaw status (<= -4.95): legal to attack in high-sec without CONCORD
    let hunter_id: i64 = 95_230_011;
    let hunter_ship: i64 = 105_230_012;
    let _ = setup_pilot_in_space(
        &ctx,
        952_311,
        hunter_id,
        98_230_003,
        hunter_ship,
        621,
        JITA_SYS,
        DVec3::new(1000.0, 0.0, 0.0),
    ).await;
    evaluate_and_record_pvp_aggression(&ctx, hunter_id, hunter_ship, JITA_SYS, ship_id, false)
        .expect("attacking outlaw is legal");
    let now = ctx.now_unix();
    assert!(!ctx.crimewatch.write().unwrap().get_or_create(hunter_id).is_criminal(now));
}

#[tokio::test]
async fn test_b23_weapon_timer_scope_and_pvp_logoff_renewal() {
    let ctx = ServerContext::new();
    let char_id: i64 = 95_230_020;
    let ship_id: i64 = 105_230_020;
    let mut sess = setup_pilot_in_space(&ctx, 952_320, char_id, 98_230_004, ship_id, 621, UEDAMA_SYS, DVec3::ZERO).await;
    let now = ctx.now_unix();
    ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now);
    for act in [
        "stargate_jump",
        "dock",
        "acceleration_gate",
        "eject",
        "store_ship",
        "refit",
        "switch_ship",
        "tether",
    ] {
        assert!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, UEDAMA_SYS, act, now)
                .is_err(),
            "weapon timer must block {act}"
        );
    }
    // Disconnected ship PvP logoff timer renewal vs NPC timer non-renewal
    let victim_id: i64 = 95_230_021;
    let _ = setup_pilot_in_space(
        &ctx,
        952_321,
        victim_id,
        98_230_005,
        105_230_021,
        621,
        UEDAMA_SYS,
        DVec3::new(500.0, 0.0, 0.0),
    ).await;
    let disc = eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, victim_id).await
        .expect("disconnect in space");
    let initial_rem = disc.remove_at_unix;

    eve_server::sp6_lifecycle::record_npc_aggression_timer(&ctx, victim_id, now + 10);
    let rem_after_npc = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships[&victim_id].remove_at_unix
    });
    assert_eq!(rem_after_npc, initial_rem);

    eve_server::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
        &ctx,
        char_id,
        victim_id,
        now + 500,
    );
    let rem_after_pvp = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships[&victim_id].remove_at_unix
    });
    assert_eq!(rem_after_pvp, now + 1_400);

    // PvP timer blocks Abyssal filament activation
    let kw = HashMap::new();
    let fil_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(47_765)],
            &kw,
            None,
        ).await;
    assert_eq!(fil_err.unwrap_err().code, "PvpTimerActive");
}

#[tokio::test]
async fn test_b23_concord_response_scaling_and_highsec_criminal_restrictions() {
    let ctx = ServerContext::new();
    let _ = concord_response_delay_sec(0.5);
    for (sec, expected_delay) in [
        (0.5, 19.0),
        (0.6, 14.0),
        (0.7, 11.0),
        (0.8, 8.0),
        (0.9, 6.0),
        (1.0, 6.0),
    ] {
        let mut cw = eve_server::combat::crimewatch::CrimeWatchDirector::new();
        let ev = cw.handle_combat_aggression_with_pod(1, 2, 10, JITA_SYS, sec, false, false, 100);
        assert_eq!(
            ev,
            AggressionEvaluation::CriminalFlagTriggered {
                concord_delay_sec: expected_delay,
                arrival_unix: 100 + expected_delay as u64,
            }
        );
    }

    let crim_id: i64 = 95_230_030;
    let crim_ship: i64 = 105_230_030;
    let vic_id: i64 = 95_230_031;
    let vic_ship: i64 = 105_230_031;
    let _ = setup_pilot_in_space(&ctx, 952_330, crim_id, 98_230_006, crim_ship, 621, JITA_SYS, DVec3::ZERO).await;
    let _ = setup_pilot_in_space(
        &ctx,
        952_331,
        vic_id,
        98_230_007,
        vic_ship,
        621,
        JITA_SYS,
        DVec3::new(1000.0, 0.0, 0.0),
    ).await;

    let now = ctx.now_unix();
    let eval = ctx
        .crimewatch
        .write()
        .unwrap()
        .handle_combat_aggression_with_pod(
            crim_id,
            vic_id,
            crim_ship,
            JITA_SYS,
            0.9,
            false,
            false,
            now,
        );
    assert_eq!(
        eval,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 6.0,
            arrival_unix: now + 6,
        }
    );

    for act in [
        "warp",
        "stargate_jump",
        "dock",
        "eject",
        "store_ship",
        "switch_ship",
    ] {
        assert!(
            check_crimewatch_action_restriction(&ctx, crim_id, crim_ship, JITA_SYS, act, now)
                .is_err(),
            "high-sec criminal must block {act}"
        );
    }

    let mut cw_director = eve_server::combat::crimewatch::CrimeWatchDirector::new();
    let _ = cw_director.handle_combat_aggression_with_pod(
        crim_id,
        vic_id,
        crim_ship,
        JITA_SYS,
        0.9,
        false,
        false,
        now,
    );
    let reports = cw_director.step_concord(&ctx, now + 1_000, 42).await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].webbed_speed_fraction, 0.0);
    assert_eq!(reports[0].warp_scramble_strength, 100);
    assert_eq!(reports[0].capacitor_after_neut, 0.0);
    assert!(reports[0].ecm_jammed);
    assert!(reports[0].death_result.is_ok());

    let low_att: i64 = 95_230_032;
    let low_vic: i64 = 95_230_033;
    let e_ship = ctx
        .crimewatch
        .write()
        .unwrap()
        .handle_combat_aggression_with_pod(
            low_att,
            low_vic,
            105_230_032,
            TAMA_SYS,
            0.3,
            false,
            false,
            now,
        );
    assert_eq!(e_ship, AggressionEvaluation::SuspectFlagTriggered);

    let e_pod = ctx
        .crimewatch
        .write()
        .unwrap()
        .handle_combat_aggression_with_pod(
            low_att,
            low_vic,
            105_230_032,
            TAMA_SYS,
            0.3,
            false,
            true,
            now,
        );
    assert_eq!(
        e_pod,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 0.0,
            arrival_unix: now,
        }
    );
}

#[tokio::test]
async fn test_b23_suspect_looting_ess_and_criminal_wreck_auto_abandon() {
    let ctx = ServerContext::new();
    let thief_id: i64 = 95_230_040;
    let thief_ship: i64 = 105_230_040;
    let owner_id: i64 = 95_230_041;
    let mut thief_sess = setup_pilot_in_space(&ctx, 952_340, thief_id, 98_230_008, thief_ship, 621, HED_GP_SYS, DVec3::ZERO).await;
    let _ = setup_pilot_in_space(&ctx, 952_341, owner_id, 98_230_009, 105_230_041, 621, HED_GP_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;
    let ess_ball_id: i64 = 700_099;
    let _ = ctx.systems.get_or_spawn(HED_GP_SYS).spawn_ball(eve_destiny::Ball {
        id: ess_ball_id,
        type_id: 32882,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 1.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;

    ctx.db.with_mut_snapshot(|s| {
        s.system_states.entry(HED_GP_SYS).or_default().ess_main_bank = IskCents::from_isk(10_000_000);
    });
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(
            &mut thief_sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_ball_id)],
            &kw,
            None,
        ).await
        .expect("AttemptLinkToMainBank");
    let now = ctx.now_unix();
    assert!(ctx.crimewatch.read().unwrap().is_suspect(thief_id, now));

    let looter_id: i64 = 95_230_042;
    let looter_ship: i64 = 105_230_042;
    let mut looter_sess = setup_pilot_in_space(&ctx, 952_342, looter_id, 98_230_010, looter_ship, 621, JITA_SYS, DVec3::ZERO).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(looter_id, SafetyLevel::Green);
    });
    ctx.crimewatch
        .write()
        .unwrap()
        .get_or_create(owner_id)
        .criminal_flag_until_unix = now + 900;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&owner_id).unwrap().solar_system_id = JITA_SYS;
        s.items.insert(
            700_040,
            item_rec(700_040, 23, owner_id, JITA_SYS as i64, InventoryFlag::None, 1, true),
        );
        s.items.insert(
            700_041,
            item_rec(700_041, 34, owner_id, 700_040, InventoryFlag::Cargo, 100, false),
        );
    });
    let loot_res = ctx
        .dispatch_rpc(
            &mut looter_sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(700_041),
                PyRep::Integer(700_040),
                PyRep::Integer(InventoryFlag::Cargo as i64),
                PyRep::Integer(50),
                PyRep::Integer(looter_ship),
            ],
            &kw,
            None,
        ).await;
    assert!(loot_res.is_ok(), "criminal jetcan is auto-abandoned and lootable under Green safety");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&700_041].quantity), 50);
    assert!(!ctx.crimewatch.read().unwrap().is_suspect(looter_id, now));
}

#[tokio::test]
async fn test_b23_limited_engagement_duel_safety_and_third_party_assistance() {
    let ctx = ServerContext::new();
    let p1_id: i64 = 95_230_050;
    let p1_ship: i64 = 105_230_050;
    let p2_id: i64 = 95_230_051;
    let p2_ship: i64 = 105_230_051;
    let logi_id: i64 = 95_230_052;
    let logi_ship: i64 = 105_230_052;

    let mut p1_sess = setup_pilot_in_space(&ctx, 952_350, p1_id, 98_230_011, p1_ship, 621, AMARR_SYS, DVec3::ZERO).await;
    let mut p2_sess = setup_pilot_in_space(
        &ctx,
        952_351,
        p2_id,
        98_230_012,
        p2_ship,
        621,
        AMARR_SYS,
        DVec3::new(1000.0, 0.0, 0.0),
    ).await;
    let _ = setup_pilot_in_space(
        &ctx,
        952_352,
        logi_id,
        98_230_013,
        logi_ship,
        621,
        AMARR_SYS,
        DVec3::new(1500.0, 0.0, 0.0),
    ).await;

    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(
            &mut p1_sess,
            "crimewatch",
            None,
            "StartDuelChallenge",
            &[PyRep::Integer(p2_id)],
            &kw,
            None,
        ).await
        .expect("StartDuelChallenge");
    let resp = ctx
        .dispatch_rpc(
            &mut p2_sess,
            "crimewatch",
            None,
            "RespondToDuelChallenge",
            &[PyRep::Integer(p1_id), PyRep::Bool(true)],
            &kw,
            None,
        ).await
        .expect("RespondToDuelChallenge");
    assert_eq!(
        dict_get(&resp, "limitedEngagementSeconds").and_then(|v| v.as_i64()),
        Some(300)
    );

    evaluate_and_record_pvp_aggression(&ctx, p1_id, p1_ship, AMARR_SYS, p2_ship, false)
        .expect("duel combat is legal");
    let now = ctx.now_unix();
    assert!(!ctx.crimewatch.write().unwrap().get_or_create(p1_id).is_criminal(now));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi_id, SafetyLevel::Green);
    });
    let green_err = evaluate_and_record_remote_assistance(&ctx, logi_id, logi_ship, AMARR_SYS, p1_ship);
    assert_eq!(green_err.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi_id, SafetyLevel::Red);
    });
    evaluate_and_record_remote_assistance(&ctx, logi_id, logi_ship, AMARR_SYS, p1_ship)
        .expect("Red safety allows remote assistance");
    assert!(ctx.crimewatch.write().unwrap().get_or_create(logi_id).is_criminal(now));
}
