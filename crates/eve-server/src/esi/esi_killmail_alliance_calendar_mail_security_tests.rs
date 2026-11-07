//! Round-7 ESI Security Regression Tests (`#466`, `#467`, `#468`, `#469`).

use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use crate::esi::router::EsiRouter;
use crate::esi::EsiHttpRequest;
use eve_db::{
    corp_roles, AllianceRecord, CalendarEventRecord, CorpGovState, CorpMemberRoleAssignment,
    CorpTitleEntry, DetailedKillmail, InventoryFlag, InventoryItemRecord, IskCents,
    KillmailAttackerEntry, KillmailItemEntry, KillmailRecord,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn issue_token(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    scopes: Vec<&str>,
) -> String {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
        } else if let Some(mut tpl) = snap.characters.get(&90_000_001).cloned() {
            tpl.character_id = char_id;
            tpl.name = format!("Pilot {char_id}");
            tpl.user_id = 1;
            tpl.corporation_id = corp_id;
            tpl.alliance_id = alliance_id;
            snap.characters.insert(char_id, tpl);
        }
    });
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().map(str::to_string).collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    ctx.db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token
}

fn call_route(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    if let Some(b) = body {
        req = req.with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    (resp.status_code, resp.json().unwrap_or(Value::Null))
}

#[test]
fn r7_466_killmail_detail_never_leaks_victim_live_inventory_or_position() {
    let (ctx, _token, attacker_id, corp_id, _planet_id) = seed_full_esi_context();
    let (victim_id, km1_id, km2_id, live_ship_id) = (90_000_002_i64, 880_001_i64, 880_002_i64, 990_880_100_i64);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.active_ship_id = live_ship_id;
        }
        for (item_id, loc, flag, type_id, quantity, singleton) in [
            (live_ship_id, 30_000_142_i64, InventoryFlag::Hangar, 587_u32, 1_i32, true),
            (live_ship_id + 1, live_ship_id, InventoryFlag::Cargo, 44_992, 500, false),
            (live_ship_id + 2, live_ship_id, InventoryFlag::HiSlot0, 14_242, 1, true),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    owner_id: victim_id,
                    location_id: loc,
                    flag,
                    type_id,
                    quantity,
                    singleton,
                    ..Default::default()
                },
            );
        }
        snap.item_positions.insert(live_ship_id, [999_999.0, 888_888.0, 777_777.0]);

        snap.killmails.insert(
            km1_id,
            KillmailRecord {
                kill_id: km1_id,
                victim_character_id: victim_id,
                attacker_character_id: attacker_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                killed_unix: now.saturating_sub(600),
                total_value_isk: 1_500_000.0,
                ..Default::default()
            },
        );
        snap.combat_crimewatch_state.detailed_killmails.insert(
            km1_id,
            DetailedKillmail {
                kill_id: km1_id,
                kill_time_unix: now.saturating_sub(600),
                solar_system_id: 30_000_142,
                victim_character_id: victim_id,
                victim_corporation_id: corp_id,
                victim_alliance_id: None,
                victim_ship_type_id: 587,
                victim_damage_taken: 1_250.0,
                victim_position: [12_500.0, -4_000.0, 8_500.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: attacker_id,
                    corporation_id: corp_id,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 484,
                    damage_done: 1_250.0,
                    final_blow: true,
                    security_status: 2.5,
                }],
                dropped_items: vec![KillmailItemEntry {
                    type_id: 484,
                    flag: InventoryFlag::HiSlot0 as u16,
                    qty_dropped: 1,
                    qty_destroyed: 0,
                    singleton: true,
                }],
                destroyed_items: vec![KillmailItemEntry {
                    type_id: 34,
                    flag: InventoryFlag::Cargo as u16,
                    qty_dropped: 0,
                    qty_destroyed: 250,
                    singleton: false,
                }],
                total_value_isk: 1_500_000.0,
                killmail_hash: String::new(),
            },
        );
        snap.killmails.insert(
            km2_id,
            KillmailRecord {
                kill_id: km2_id,
                victim_character_id: victim_id,
                attacker_character_id: attacker_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                killed_unix: now.saturating_sub(300),
                total_value_isk: 500_000.0,
                ..Default::default()
            },
        );
    });

    let router = EsiRouter::new(Arc::clone(&ctx));
    let hash1 = crate::esi::sample_routes::compute_killmail_hash(km1_id, victim_id, attacker_id, 587);
    let (st1, km1) = call_route(&router, "GET", &format!("/v1/killmails/{km1_id}/{hash1}/"), None, None);
    assert_eq!(st1, 200);
    assert_eq!(km1["victim"]["position"], json!({"x": 12_500.0, "y": -4_000.0, "z": 8_500.0}));
    let items1 = km1["victim"]["items"].as_array().unwrap();
    assert_eq!(items1.len(), 2);
    assert!(items1.iter().any(|i| i["item_type_id"] == json!(484) && i["quantity_dropped"] == json!(1)));
    assert!(items1.iter().any(|i| i["item_type_id"] == json!(34) && i["quantity_destroyed"] == json!(250)));
    assert!(!items1.iter().any(|i| i["item_type_id"] == json!(44_992) || i["item_type_id"] == json!(14_242)));

    let hash2 = crate::esi::sample_routes::compute_killmail_hash(km2_id, victim_id, attacker_id, 587);
    let (st2, km2) = call_route(&router, "GET", &format!("/v1/killmails/{km2_id}/{hash2}/"), None, None);
    assert_eq!(st2, 200);
    assert_eq!(km2["victim"]["items"], json!([]));
    assert_eq!(km2["victim"]["position"], json!({"x": 0.0, "y": 0.0, "z": 0.0}));
}

#[test]
fn r7_467_empty_alliance_membership_and_pending_grantable_roles_enforced() {
    let (ctx, _token, _char1, corp_id, _planet_id) = seed_full_esi_context();
    let (outsider_id, trainee_id, empty_alliance_id) = (90_000_002_i64, 90_000_003_i64, 99_500_001_i64);
    let now = ctx.now_unix();

    let tok_outsider = issue_token(
        &ctx,
        outsider_id,
        1_000_035,
        None,
        vec!["esi-alliances.read_contacts.v1", "esi-wallet.read_corporation_wallets.v1"],
    );
    let tok_trainee = issue_token(
        &ctx,
        trainee_id,
        corp_id,
        Some(99_000_001),
        vec![
            "esi-wallet.read_corporation_wallets.v1",
            "esi-characters.read_corporation_roles.v1",
            "esi-corporations.read_structures.v1",
            "esi-markets.read_corporation_orders.v1",
        ],
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            empty_alliance_id,
            AllianceRecord {
                alliance_id: empty_alliance_id,
                name: "Empty Alliance".into(),
                short_name: "EMPTY".into(),
                executor_corp_id: 0,
                member_corp_ids: HashSet::new(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&outsider_id) {
            ch.alliance_id = None;
            ch.corporation_id = 1_000_035;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(trainee_id);
            corp.member_roles.insert(trainee_id, 0);
        }
        snap.corp_role_assignments.insert(
            (corp_id, trainee_id),
            CorpMemberRoleAssignment {
                character_id: trainee_id,
                roles: 0,
                grantable_roles: corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
                grantable_roles_at_hq: corp_roles::ROLE_STATION_MANAGER,
                ..Default::default()
            },
        );
    });

    let router = EsiRouter::new(Arc::clone(&ctx));
    assert_eq!(call_route(&router, "GET", &format!("/v1/alliances/{empty_alliance_id}/contacts/"), Some(&tok_outsider), None).0, 403);

    for path in [
        format!("/v1/corporations/{corp_id}/wallets/"),
        format!("/v1/corporations/{corp_id}/structures/"),
        format!("/v1/corporations/{corp_id}/orders/"),
        format!("/v1/corporations/{corp_id}/roles/history/"),
    ] {
        assert_eq!(call_route(&router, "GET", &path, Some(&tok_trainee), None).0, 403, "#467: grantable_roles must not unlock {path}");
    }

    // Unmatured 24h pending role & title role grants must be masked until maturation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(trainee_id, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT);
        }
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry { title_id: 1, name: "PendingTitle".into(), roles: vec!["Accountant".into()], ..Default::default() }],
        );
        snap.corp_member_titles.insert((corp_id, trainee_id), vec![1]);
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants.insert(trainee_id, (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT, now + 86_400));
        gov.pending_title_role_grants.insert(trainee_id, (corp_roles::ROLE_ACCOUNTANT, now + 86_400));
    });
    assert_eq!(call_route(&router, "GET", &format!("/v1/corporations/{corp_id}/wallets/"), Some(&tok_trainee), None).0, 403);
    let (st_roles, roles_json) = call_route(&router, "GET", &format!("/v1/characters/{trainee_id}/roles/"), Some(&tok_trainee), None);
    assert_eq!(st_roles, 200);
    assert!(!roles_json["roles"].as_array().unwrap().iter().any(|r| r == "Director" || r == "Accountant"));

    // Matured role grant unlocks corp wallet access
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants.insert(trainee_id, (corp_roles::ROLE_ACCOUNTANT, now));
        gov.pending_title_role_grants.insert(trainee_id, (corp_roles::ROLE_ACCOUNTANT, now));
    });
    assert_eq!(call_route(&router, "GET", &format!("/v1/corporations/{corp_id}/wallets/"), Some(&tok_trainee), None).0, 200);
}

#[test]
fn r7_468_calendar_event_attendees_restricted_to_owner_or_leadership() {
    let (ctx, _token, creator_id, corp_id, _planet_id) = seed_full_esi_context();
    let (invitee_id, line_member_id) = (90_000_002_i64, 90_000_003_i64);
    let (personal_ev_id, corp_ev_id, ccp_ev_id) = (950_001_i64, 950_002_i64, 950_003_i64);

    let tok_creator = issue_token(&ctx, creator_id, corp_id, Some(99_000_001), vec!["esi-calendar.read_calendar_events.v1"]);
    let tok_invitee = issue_token(&ctx, invitee_id, corp_id, Some(99_000_001), vec!["esi-calendar.read_calendar_events.v1"]);
    let tok_member = issue_token(&ctx, line_member_id, corp_id, Some(99_000_001), vec!["esi-calendar.read_calendar_events.v1"]);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(line_member_id);
            corp.member_roles.insert(line_member_id, 0);
        }
        snap.calendar_events.insert(
            personal_ev_id,
            CalendarEventRecord {
                event_id: personal_ev_id,
                owner_id: creator_id,
                creator_id,
                event_date_time: 1_700_010_000,
                duration: 60,
                importance: 1,
                title: "Secret Fleet Op".into(),
                description: "Opsec".into(),
                scope: "Personal".into(),
                responses: HashMap::from([(creator_id, 4), (invitee_id, 2)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            corp_ev_id,
            CalendarEventRecord {
                event_id: corp_ev_id,
                owner_id: corp_id,
                creator_id,
                event_date_time: 1_700_020_000,
                duration: 60,
                importance: 0,
                title: "Corp Meeting".into(),
                description: "Officers only".into(),
                scope: "Corporation".into(),
                responses: HashMap::from([(creator_id, 4), (line_member_id, 4)]),
                is_deleted: false,
            },
        );
        snap.calendar_events.insert(
            ccp_ev_id,
            CalendarEventRecord {
                event_id: ccp_ev_id,
                owner_id: 1,
                creator_id: 1,
                event_date_time: 1_700_030_000,
                duration: 120,
                importance: 1,
                title: "EVE Fanfest".into(),
                description: "Global event".into(),
                scope: "CCP".into(),
                responses: HashMap::from([(invitee_id, 4)]),
                is_deleted: false,
            },
        );
    });

    let router = EsiRouter::new(Arc::clone(&ctx));
    let (st_owner, owner_att) = call_route(&router, "GET", &format!("/v1/characters/{creator_id}/calendar/{personal_ev_id}/attendees/"), Some(&tok_creator), None);
    assert_eq!(st_owner, 200);
    assert_eq!(owner_att.as_array().unwrap().len(), 2);
    assert_eq!(call_route(&router, "GET", &format!("/v1/characters/{invitee_id}/calendar/{personal_ev_id}/attendees/"), Some(&tok_invitee), None).0, 403);
    assert_eq!(call_route(&router, "GET", &format!("/v1/characters/{line_member_id}/calendar/{corp_ev_id}/attendees/"), Some(&tok_member), None).0, 403);
    let (st_corp_dir, corp_att) = call_route(&router, "GET", &format!("/v1/characters/{creator_id}/calendar/{corp_ev_id}/attendees/"), Some(&tok_creator), None);
    assert_eq!(st_corp_dir, 200);
    assert_eq!(corp_att.as_array().unwrap().len(), 2);
    assert_eq!(call_route(&router, "GET", &format!("/v1/characters/{invitee_id}/calendar/{ccp_ev_id}/attendees/"), Some(&tok_invitee), None).0, 403);
}

#[test]
fn r7_469_mail_enforces_blocklists_corp_alliance_permissions_and_cspa() {
    let (ctx, _token, dir_id, corp_id, _planet_id) = seed_full_esi_context();
    let (stranger_id, line_member_id, alliance_id) = (90_000_002_i64, 90_000_003_i64, 99_000_001_i64);

    let tok_stranger = issue_token(&ctx, stranger_id, 1_000_035, None, vec!["esi-mail.send_mail.v1"]);
    let tok_member = issue_token(&ctx, line_member_id, corp_id, Some(alliance_id), vec!["esi-mail.send_mail.v1"]);
    let tok_dir = issue_token(&ctx, dir_id, corp_id, Some(alliance_id), vec!["esi-mail.send_mail.v1"]);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(line_member_id);
            corp.member_roles.insert(line_member_id, 0);
        }
        if let Some(ch) = snap.characters.get_mut(&stranger_id) {
            ch.corporation_id = 1_000_035;
            ch.alliance_id = None;
            ch.balance = IskCents::from_isk(100_000);
        }
        snap.character_cspa_charges.insert(line_member_id, IskCents::from_isk(2_950));
        let mut gov = CorpGovState::default();
        gov.restrict_corp_mails = true;
        snap.corp_governance.insert(corp_id, gov);
    });
    ctx.social_extras.lock().unwrap().blocked.entry(stranger_id).or_default().insert(line_member_id);

    let router = EsiRouter::new(Arc::clone(&ctx));
    assert_eq!(
        call_route(
            &router,
            "POST",
            &format!("/v1/characters/{line_member_id}/mail/"),
            Some(&tok_member),
            Some(json!({"approved_cost": 10_000, "subject": "Blocked", "body": "Fail", "recipients": [{"recipient_id": stranger_id, "recipient_type": "character"}]})),
        ).0,
        403
    );
    for (rid, rtype) in [(corp_id, "corporation"), (alliance_id, "alliance")] {
        assert_eq!(
            call_route(
                &router,
                "POST",
                &format!("/v1/characters/{stranger_id}/mail/"),
                Some(&tok_stranger),
                Some(json!({"approved_cost": 0, "subject": "Spam", "body": "Unauthorized", "recipients": [{"recipient_id": rid, "recipient_type": rtype}]})),
            ).0,
            403
        );
    }
    assert_eq!(
        call_route(
            &router,
            "POST",
            &format!("/v1/characters/{line_member_id}/mail/"),
            Some(&tok_member),
            Some(json!({"approved_cost": 0, "subject": "Corp ping", "body": "No role", "recipients": [{"recipient_id": corp_id, "recipient_type": "corporation"}]})),
        ).0,
        403
    );
    assert_eq!(
        call_route(
            &router,
            "POST",
            &format!("/v1/characters/{dir_id}/mail/"),
            Some(&tok_dir),
            Some(json!({"approved_cost": 0, "subject": "Official CTA", "body": "Form up", "recipients": [{"recipient_id": corp_id, "recipient_type": "corporation"}, {"recipient_id": alliance_id, "recipient_type": "alliance"}]})),
        ).0,
        201
    );

    let bal_before = ctx.db.with_snapshot(|snap| snap.characters.get(&stranger_id).unwrap().balance.0);
    assert_eq!(
        call_route(
            &router,
            "POST",
            &format!("/v1/characters/{stranger_id}/mail/"),
            Some(&tok_stranger),
            Some(json!({"approved_cost": 1_000, "subject": "Low CSPA", "body": "Too cheap", "recipients": [{"recipient_id": line_member_id, "recipient_type": "character"}]})),
        ).0,
        400
    );
    assert_eq!(
        call_route(
            &router,
            "POST",
            &format!("/v1/characters/{stranger_id}/mail/"),
            Some(&tok_stranger),
            Some(json!({"approved_cost": 5_000, "subject": "Paid CSPA", "body": "Paid", "recipients": [{"recipient_id": line_member_id, "recipient_type": "character"}]})),
        ).0,
        201
    );
    let bal_after = ctx.db.with_snapshot(|snap| snap.characters.get(&stranger_id).unwrap().balance.0);
    assert_eq!(bal_before - bal_after, IskCents::from_isk(2_950).0);
}
