//! Coverage tests for OpenAPI validator, legacy routes, mail/contacts/calendar CRUD, sov/FW, killmails, search, fleet errors, and HTTP transport.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::{EsiHttpRequest, EsiRouter, EsiServer, OpenApiSpec};
use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, CalendarEventRecord, CharacterMedalRecord,
    CharacterResearchAgentRecord, CharacterScopedRolesRecord, FwSystemOccupationRecord,
    IncursionRecord, InventoryFlag, InventoryItemRecord, JumpCloneRecord,
    KillmailAttackerDetail, KillmailDetailRecord, MailLabelRecord, MailRecord,
    MailingListRecord, SkillQueueEntry, SocialContactRecord, SystemJumpEvent, SystemKillEvent,
    TrainedSkillRecord,
};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[test]
fn esi_openapi_validator_legacy_social_mail_contacts_and_character_details() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char2_id = 90_000_002_i64;
    let structure_id = 1_055_000_001_i64;
    let now = ctx.now_unix();

    // 1. OpenApiSpec::validate_response & schema validation branches
    let status_resp = router.handle_request(&EsiHttpRequest::new("GET", "/latest/status"));
    assert_eq!(status_resp.status_code, 200);
    spec.validate_response(
        "GET",
        "/latest/status",
        status_resp.status_code,
        &status_resp.headers,
        &status_resp.body,
    )
    .expect("valid status schema");
    assert!(spec
        .validate_response("GET", "/latest/status", 200, &HashMap::new(), &status_resp.body)
        .is_err());
    assert!(spec
        .validate_response("GET", "/latest/status", 304, &status_resp.headers, b"nonempty")
        .is_err());
    assert!(OpenApiSpec::from_json_str("{not-json").is_err());

    // 2. Seed character details, mail, calendar, contacts, bookmarks, legacy opportunities
    let ship_item_id = 100_000_001_i64;
    let fitted_mod_id = 100_000_002_i64;
    let structure_item_id = 100_000_003_i64;
    let mail_id = 910_100_001_i64;
    let event_id = 910_200_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 3.25);
            ch.jump_fatigue_minutes = 12.5;
            ch.home_station_id = 60_003_760;
        }
        snap.item_custom_names.insert(ship_item_id, "Stiletto Prime".into());
        snap.items.insert(
            fitted_mod_id,
            InventoryItemRecord {
                item_id: fitted_mod_id,
                type_id: 2897,
                owner_id: char_id,
                location_id: ship_item_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            structure_item_id,
            InventoryItemRecord {
                item_id: structure_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.jump_clones.insert(
            char_id,
            vec![JumpCloneRecord {
                jump_clone_id: 1,
                character_id: char_id,
                location_id: 60_003_760,
                implants: vec![9899],
            }],
        );
        snap.active_implants.insert(char_id, vec![9899]);
        snap.skills.entry(char_id).or_default().insert(
            3438,
            TrainedSkillRecord {
                type_id: 3438,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3438,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 86_400.0,
                started_at_unix: Some(now.saturating_sub(600)),
            }],
        );
        snap.bookmark_folders.insert(
            501,
            BookmarkFolderRecord {
                folder_id: 501,
                owner_id: char_id,
                folder_name: "Safe Spots".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                description: "System safes".into(),
                admin_group_id: None,
                manage_group_id: None,
                use_group_id: None,
                view_group_id: None,
            },
        );
        snap.bookmarks.insert(
            601,
            BookmarkRecord {
                bookmark_id: 601,
                owner_id: char_id,
                label: "Jita Perch".into(),
                solar_system_id: 30_000_142,
                x: 100.0,
                y: 200.0,
                z: 300.0,
                item_id: None,
                memo: "Undock perch".into(),
                folder_id: Some(501),
                is_shared: true,
            },
        );
        snap.char_research_agents.insert(
            char_id,
            vec![CharacterResearchAgentRecord {
                agent_id: 3009356,
                skill_type_id: 11433,
                started_at_unix: now.saturating_sub(86_400),
                points_per_day: 120.0,
                remainder_points: 50.0,
            }],
        );
        snap.char_medals.insert(
            char_id,
            vec![CharacterMedalRecord {
                medal_id: 10,
                title: "Valor".into(),
                description: "Frontline".into(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: now,
                reason: "Brave".into(),
                status: "public".into(),
                graphics: vec![],
            }],
        );
        snap.char_scoped_roles.insert(
            char_id,
            CharacterScopedRolesRecord {
                roles: vec!["Director".into()],
                roles_at_hq: vec![],
                roles_at_base: vec![],
                roles_at_other: vec![],
            },
        );
        snap.mails.push(MailRecord {
            message_id: mail_id,
            sender_id: char2_id,
            recipient_id: char_id,
            subject: "Briefing".into(),
            body: "Dock at Jita 4-4".into(),
        });
        snap.mail_timestamps.insert(mail_id, now);
        snap.mail_labels.insert(
            char_id,
            vec![MailLabelRecord {
                label_id: 1,
                name: "Inbox".into(),
                color: "#ffffff".into(),
            }],
        );
        snap.mail_assigned_labels.insert((char_id, mail_id), vec![1]);
        snap.mailing_lists.insert(
            801,
            MailingListRecord {
                list_id: 801,
                name: "Alliance Fleet".into(),
                owner_id: char_id,
                default_access: 0,
                default_role: 0,
                cost: 0,
                members: HashMap::new(),
                entity_access: HashMap::new(),
                welcome_mail: None,
                mail_ids: HashSet::from([mail_id]),
            },
        );
        snap.calendar_events.insert(
            event_id,
            CalendarEventRecord {
                event_id,
                owner_id: corp_id,
                creator_id: char_id,
                event_date_time: now as i64 + 3600,
                duration: 60,
                importance: 1,
                title: "Stratop CTA".into(),
                description: "Form up".into(),
                scope: "corporation".into(),
                responses: HashMap::from([(char_id, 1)]),
                is_deleted: false,
            },
        );
        snap.char_contacts.insert(
            char_id,
            vec![SocialContactRecord {
                contact_id: char2_id,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: true,
                is_blocked: false,
                label_ids: vec![1],
            }],
        );
        snap.char_contact_labels.insert(char_id, vec![(1, "FCs".into())]);
    });

    for path in [
        format!("/latest/characters/{char_id}"),
        format!("/latest/characters/{char_id}/corporationhistory"),
        format!("/latest/characters/{char_id}/notifications"),
        format!("/latest/characters/{char_id}/notifications/contacts"),
        format!("/latest/characters/{char_id}/agents_research"),
        format!("/latest/characters/{char_id}/medals"),
        format!("/latest/characters/{char_id}/roles"),
        format!("/latest/characters/{char_id}/titles"),
        format!("/latest/characters/{char_id}/bookmarks"),
        format!("/latest/characters/{char_id}/bookmarks/folders"),
        format!("/latest/corporations/{corp_id}/bookmarks"),
        format!("/latest/corporations/{corp_id}/bookmarks/folders"),
        format!("/latest/corporations/{corp_id}/projects"),
        "/latest/opportunities/groups".to_string(),
        "/latest/opportunities/tasks".to_string(),
        format!("/latest/characters/{char_id}/opportunities"),
        format!("/latest/characters/{char_id}/standings"),
        format!("/latest/characters/{char_id}/fatigue"),
        format!("/latest/characters/{char_id}/blueprints"),
        format!("/latest/characters/{char_id}/clones"),
        format!("/latest/characters/{char_id}/implants"),
        format!("/latest/characters/{char_id}/skills"),
        format!("/latest/characters/{char_id}/attributes"),
        format!("/latest/characters/{char_id}/skillqueue"),
        format!("/latest/characters/{char_id}/location"),
        format!("/latest/characters/{char_id}/online"),
        format!("/latest/characters/{char_id}/ship"),
        format!("/latest/characters/{char_id}/calendar"),
        format!("/latest/characters/{char_id}/calendar/{event_id}"),
        format!("/latest/characters/{char_id}/calendar/{event_id}/attendees"),
        format!("/latest/characters/{char_id}/mail/labels"),
        format!("/latest/characters/{char_id}/mail/lists"),
        format!("/latest/characters/{char_id}/mail/{mail_id}"),
        format!("/latest/characters/{char_id}/contacts"),
        format!("/latest/characters/{char_id}/contacts/labels"),
        format!("/latest/corporations/{corp_id}/contacts"),
        format!("/latest/corporations/{corp_id}/contacts/labels"),
        "/latest/alliances/99000001/contacts".to_string(),
        "/latest/alliances/99000001/contacts/labels".to_string(),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", &path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
    }

    // Asset locations & names + CSPA + calendar PUT + mail & contact mutations
    let asset_ids = json!([ship_item_id, fitted_mod_id, structure_item_id]);
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/assets/locations"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&asset_ids).unwrap()),
            )
            .status_code,
        200
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/assets/names"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&asset_ids).unwrap()),
            )
            .status_code,
        200
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", format!("/latest/characters/{char_id}/cspa"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&json!([char2_id])).unwrap()),
            )
            .status_code,
        201
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("PUT", format!("/latest/characters/{char_id}/calendar/{event_id}"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&json!({"response": "accepted"})).unwrap()),
            )
            .status_code,
        204
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("PUT", format!("/latest/characters/{char_id}/mail/{mail_id}"))
                    .with_bearer_token(&token)
                    .with_body(serde_json::to_vec(&json!({"read": true, "labels": [1]})).unwrap()),
            )
            .status_code,
        204
    );
}

#[tokio::test]
async fn esi_sov_fw_killmails_search_fleet_errors_and_http_roundtrip() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let char2_id = 90_000_002_i64;
    let structure_id = 1_055_000_001_i64;
    let now = ctx.now_unix();
    let tcu_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            ctx.sde
                .groups
                .get(&t.group_id)
                .is_some_and(|g| g.name == "Territorial Claim Unit")
        })
        .map(|t| t.type_id)
        .unwrap_or(32_226);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.type_id = tcu_tid;
        }
        snap.structure_vulnerability_windows
            .insert(structure_id, (now.saturating_sub(300), now + 3_600));
        snap.incursions.insert(
            20_000_020,
            IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142, 30_000_144],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.75,
                state: "established".into(),
                incursion_type: "Incursion".into(),
            },
        );
        snap.fw_system_occupations.insert(
            30_002_758,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_758,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004,
                victory_points: 15_000,
                victory_points_threshold: 30_000,
            },
        );
        snap.system_jump_events.push(SystemJumpEvent {
            solar_system_id: 30_000_142,
            character_id: char_id,
            timestamp_unix: now,
        });
        snap.system_kill_events.push(SystemKillEvent {
            solar_system_id: 30_000_142,
            ship_kills: 2,
            npc_kills: 15,
            pod_kills: 1,
            timestamp_unix: now,
        });
        snap.killmail_details.insert(
            55_001,
            KillmailDetailRecord {
                kill_id: 55_001,
                killmail_time_unix: now,
                damage_taken: 1_250,
                victim_position: Some([100.0, 200.0, 300.0]),
                attackers: vec![KillmailAttackerDetail {
                    character_id: char_id,
                    corporation_id: corp_id,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 587,
                    weapon_type_id: 2897,
                    damage_done: 1_250,
                    final_blow: true,
                    security_status: 5.0,
                }],
            },
        );
    });

    // Fetch real killmail hash then request full killmail detail
    let recent_kms = router
        .handle_request(
            &EsiHttpRequest::new("GET", format!("/latest/characters/{char_id}/killmails/recent"))
                .with_bearer_token(&token),
        )
        .json()
        .unwrap();
    let km_hash = recent_kms[0]["killmail_hash"].as_str().unwrap();

    for path in [
        "/latest/sovereignty/map".to_string(),
        "/latest/sovereignty/structures".to_string(),
        "/latest/sovereignty/campaigns".to_string(),
        "/latest/incursions".to_string(),
        "/latest/fw/wars".to_string(),
        "/latest/fw/stats".to_string(),
        "/latest/fw/systems".to_string(),
        format!("/latest/characters/{char_id}/fw/stats"),
        format!("/latest/corporations/{corp_id}/fw/stats"),
        "/latest/universe/system_jumps".to_string(),
        "/latest/universe/system_kills".to_string(),
        format!("/latest/killmails/55001/{km_hash}"),
        "/latest/wars/44001".to_string(),
        "/latest/wars/44001/killmails".to_string(),
        "/latest/route/30000142/30000144?flag=insecure".to_string(),
        "/latest/route/30000142/30000144?flag=shortest".to_string(),
        "/latest/universe/structures?filter=market".to_string(),
        format!(
            "/latest/characters/{char_id}/search?categories=agent,alliance,character,constellation,corporation,faction,inventory_type,region,solar_system,station&search=Jita"
        ),
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", &path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
    }

    // OpenWindow UI endpoints & fleet error paths
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 998, Some(char_id), false, now);
    let info_owner_path = format!("/latest/ui/openwindow/information?target_id={char2_id}");
    for ui_path in [
        "/latest/ui/openwindow/contract?contract_id=66001",
        info_owner_path.as_str(),
        "/latest/ui/openwindow/marketdetails?type_id=34",
    ] {
        assert_eq!(
            router
                .handle_request(&EsiHttpRequest::new("POST", ui_path).with_bearer_token(&token))
                .status_code,
            204
        );
    }
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/latest/ui/openwindow/information?target_id=587")
                    .with_bearer_token(&token)
            )
            .status_code,
        501
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/latest/ui/openwindow/newmail")
                    .with_bearer_token(&token)
                    .with_body(
                        serde_json::to_vec(&json!({
                            "body": "Hello",
                            "recipients": [char2_id],
                            "subject": "Ping"
                        }))
                        .unwrap()
                    )
            )
            .status_code,
        204
    );
    assert_eq!(
        router
            .handle_request(
                &EsiHttpRequest::new("POST", "/latest/fleets/999999/members")
                    .with_bearer_token(&token)
                    .with_body(
                        serde_json::to_vec(&json!({
                            "character_id": char2_id,
                            "role": "squad_member"
                        }))
                        .unwrap()
                    )
            )
            .status_code,
        404
    );

    // Live HTTP/1.1 TCP roundtrip exercising EsiServer + send_raw_esi_request
    let (addr, handle) = EsiServer::spawn_with_router(Arc::new(router), "127.0.0.1:0")
        .await
        .expect("spawn ESI HTTP");
    let http_resp = super::send_raw_esi_request(
        addr,
        &EsiHttpRequest::new("GET", "/latest/status"),
    )
    .await
    .expect("raw HTTP request");
    assert_eq!(http_resp.status_code, 200);
    handle.abort();
}
