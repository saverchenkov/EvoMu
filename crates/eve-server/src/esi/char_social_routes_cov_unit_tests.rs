//! Deep branch-coverage and mutation-killing unit tests for `esi::char_social_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use crate::esi::runtime_state::{EsiContactEntry, EsiContactNotification};
use crate::router::cache_notifications::NotificationRecord;
use eve_db::durable_domain_types::CorpGovState;
use eve_db::{
    AllianceRecord, BookmarkFolderRecord, BookmarkRecord, CharacterMedalGraphicRecord,
    CharacterMedalRecord, CharacterResearchAgentRecord, CharacterScopedRolesRecord, CorpTitleEntry,
    IskCents,
};
use serde_json::json;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

#[test]
fn infer_contact_type_boundaries_and_legacy_operation_matching_edges() {
    let (ctx, _tok, _char_id, _corp_id, _pid) = seed_full_esi_context();
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            77_777_777,
            AllianceRecord {
                alliance_id: 77_777_777,
                name: "Custom Alliance".into(),
                short_name: "CSTA".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: Default::default(),
            },
        );
    });
    let sde_faction_id = ctx.sde.factions.keys().copied().next().unwrap_or(500_001) as i64;
    let sde_npc_corp_id = ctx.sde.npc_corporations.keys().copied().next().unwrap_or(1_000_009);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(infer_contact_type(&ctx, snap, 499_999), "character");
        assert_eq!(infer_contact_type(&ctx, snap, 500_000), "faction");
        assert_eq!(infer_contact_type(&ctx, snap, 600_000), "character");
        assert_eq!(infer_contact_type(&ctx, snap, sde_faction_id), "faction");
        assert_eq!(infer_contact_type(&ctx, snap, 77_777_777), "alliance");
        assert_eq!(infer_contact_type(&ctx, snap, 99_000_000), "alliance");
        assert_eq!(infer_contact_type(&ctx, snap, 100_000_000), "character");
        assert_eq!(infer_contact_type(&ctx, snap, sde_npc_corp_id), "corporation");
        assert_eq!(infer_contact_type(&ctx, snap, 1_000_000), "corporation");
        assert_eq!(infer_contact_type(&ctx, snap, 2_000_000), "character");
        assert_eq!(infer_contact_type(&ctx, snap, 98_000_000), "corporation");
    });

    assert!(try_match_legacy_operation("GET", "").is_none());
    assert!(try_match_legacy_operation("GET", "/v1").is_none());
    assert!(try_match_legacy_operation("GET", "/v1/").is_none());
    assert!(try_match_legacy_operation("GET", "/v10/opportunities/groups").is_none());
    assert!(try_match_legacy_operation("GET", "/opportunities/wrong").is_none());

    for (raw, expected_op) in [
        ("/v1/characters/10/bookmarks", "GetCharactersCharacterIdBookmarks"),
        ("/characters/10/bookmarks/folders", "GetCharactersCharacterIdBookmarkFolders"),
        ("/corporations/20/bookmarks", "GetCorporationsCorporationIdBookmarks"),
        ("/corporations/20/bookmarks/folders", "GetCorporationsCorporationIdBookmarkFolders"),
        ("/corporations/20/projects", "GetCorporationsCorporationIdProjects"),
        ("/opportunities/groups", "GetOpportunitiesGroups"),
        ("/opportunities/tasks", "GetOpportunitiesTasks"),
        ("/characters/10/opportunities", "GetCharactersCharacterIdOpportunities"),
    ] {
        let m = try_match_legacy_operation("get", raw).expect(raw);
        assert_eq!(m.operation.operation_id, expected_op);
    }
}

#[test]
fn character_public_history_fallbacks_cspa_exemptions_and_notifications() {
    let (ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let empty = HashMap::new();
    let char_s = char_id.to_string();
    let p_char = params(&[("character_id", &char_s)]);

    let npc_corp_with_faction = ctx
        .sde
        .npc_corporations
        .iter()
        .find(|(_, c)| c.faction_id.is_some())
        .map(|(&id, c)| (id, c.faction_id.unwrap() as i64));

    let c_female = 90_800_001_i64;
    let c_male_plain = 90_800_002_i64;
    let c_same_corp = 90_800_003_i64;
    let c_exempt_contact = 90_800_004_i64;
    let c_terrible_contact = 90_800_005_i64;
    let c_blocked_contact = 90_800_006_i64;
    let c_neg_cspa = 90_800_007_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&char_id].clone();
        let mut f = tpl.clone();
        f.character_id = c_female;
        f.gender = 0;
        f.title = "Admiral".into();
        f.alliance_id = Some(99_000_001);
        f.war_faction_id = None;
        if let Some((ncorp, _)) = npc_corp_with_faction {
            f.corporation_id = ncorp;
        } else {
            f.war_faction_id = Some(500_001);
        }
        f.create_date_unix = 1_650_000_000;
        snap.characters.insert(c_female, f);

        let mut m = tpl.clone();
        m.character_id = c_male_plain;
        m.gender = 1;
        m.title = String::new();
        m.alliance_id = None;
        m.war_faction_id = None;
        m.corporation_id = corp_id;
        m.create_date_unix = 0;
        snap.characters.insert(c_male_plain, m);

        let mut sc = tpl.clone();
        sc.character_id = c_same_corp;
        sc.corporation_id = corp_id;
        snap.characters.insert(c_same_corp, sc);

        for tid in [c_exempt_contact, c_terrible_contact, c_blocked_contact, c_neg_cspa] {
            let mut t = tpl.clone();
            t.character_id = tid;
            t.corporation_id = 98_999_999;
            snap.characters.insert(tid, t);
        }

        snap.employment_history
            .insert(c_female, vec![(1_000_009, 1_600_000_000, None), (corp_id, 1_650_000_000, None)]);
        snap.employment_history.remove(&char_id);
        snap.employment_history.remove(&c_male_plain);

        // CSPA charges
        snap.character_cspa_charges.insert(char_id, IskCents::from_isk(9_999));
        snap.character_cspa_charges.insert(c_same_corp, IskCents::from_isk(8_888));
        snap.character_cspa_charges.insert(c_exempt_contact, IskCents::from_isk(7_777));
        snap.character_cspa_charges.insert(c_terrible_contact, IskCents::from_isk(1_000));
        snap.character_cspa_charges.insert(c_blocked_contact, IskCents::from_isk(2_500));
        snap.character_cspa_charges.insert(c_neg_cspa, IskCents(-500));
    });

    {
        let mut contacts = ctx.esi_state.char_contacts.write().unwrap();
        contacts.insert(
            c_exempt_contact,
            vec![EsiContactEntry {
                contact_id: char_id,
                contact_type: "character".into(),
                standing: -9.9, // > -10.0 -> exempt!
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
        contacts.insert(
            c_terrible_contact,
            vec![EsiContactEntry {
                contact_id: char_id,
                contact_type: "character".into(),
                standing: -10.0, // == -10.0 -> NOT exempt!
                is_watched: false,
                is_blocked: false,
                label_ids: vec![],
            }],
        );
        contacts.insert(
            c_blocked_contact,
            vec![EsiContactEntry {
                contact_id: char_id,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: false,
                is_blocked: true, // blocked -> NOT exempt!
                label_ids: vec![],
            }],
        );
    }

    // 1. Public character info branches
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}", &empty, &empty, b"").unwrap().status_code, 400);
    let r_fem = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}", &params(&[("character_id", &c_female.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&r_fem)["gender"], "female");
    assert_eq!(body(&r_fem)["title"], "Admiral");
    assert_eq!(body(&r_fem)["alliance_id"], 99_000_001);
    assert!(body(&r_fem).get("faction_id").is_some());

    let r_male = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}", &params(&[("character_id", &c_male_plain.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&r_male)["gender"], "male");
    assert!(body(&r_male).get("title").is_none() && body(&r_male).get("alliance_id").is_none() && body(&r_male).get("faction_id").is_none());

    // 2. Corporation history fallbacks: employment_history, create_date_unix > 0, create_date_unix == 0
    ctx.esi_state.char_corp_history.write().unwrap().clear();
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/corporationhistory", &empty, &empty, b"").unwrap().status_code, 400);
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/corporationhistory", &params(&[("character_id", "99999999")]), &empty, b"").unwrap().status_code, 404);

    let h_emp = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/corporationhistory", &params(&[("character_id", &c_female.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&h_emp).as_array().unwrap().len(), 2);
    assert_eq!(body(&h_emp)[0]["record_id"], 2);

    let h_birth = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/corporationhistory", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&h_birth).as_array().unwrap().len(), 1);
    assert_eq!(body(&h_birth)[0]["record_id"], 1);

    let h_zero = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/corporationhistory", &params(&[("character_id", &c_male_plain.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&h_zero), &json!([]));

    // 3. CSPA charges: bad param, 404, 100-item boundary, and exact exemption math (1000 + 2500 = 3500 ISK)
    assert_eq!(dispatch_char_social_route(&ctx, "POST", "/characters/{character_id}/cspa", &empty, &empty, b"[1]").unwrap().status_code, 400);
    assert_eq!(dispatch_char_social_route(&ctx, "POST", "/characters/{character_id}/cspa", &params(&[("character_id", "99999999")]), &empty, b"[1]").unwrap().status_code, 404);
    let exact_100: Vec<i64> = vec![char_id; 100];
    let r_100 = dispatch_char_social_route(&ctx, "POST", "/characters/{character_id}/cspa", &p_char, &empty, &serde_json::to_vec(&exact_100).unwrap()).unwrap();
    assert_eq!(r_100.status_code, 201);
    assert_eq!(body(&r_100).as_f64().unwrap(), 0.0);

    let cspa_targets = vec![char_id, c_same_corp, c_exempt_contact, c_terrible_contact, c_blocked_contact, c_neg_cspa];
    let r_cspa = dispatch_char_social_route(&ctx, "POST", "/characters/{character_id}/cspa", &p_char, &empty, &serde_json::to_vec(&cspa_targets).unwrap()).unwrap();
    assert_eq!(body(&r_cspa).as_f64().unwrap(), 3_500.0);

    // 4. Notifications & Contact Notifications
    {
        let mut recs = ctx.notification_records.write().unwrap();
        recs.clear();
        recs.push(NotificationRecord {
            notification_id: 10,
            type_id: 10,
            group_id: 1,
            sender_id: 500_001,
            receiver_id: char_id,
            processed: true,
            created: 0, // < epoch -> clamps to 0
            data: "bounty".into(),
            deleted: false,
            interacted: false,
        });
        recs.push(NotificationRecord {
            notification_id: 20,
            type_id: 184,
            group_id: 1,
            sender_id: 99_000_001,
            receiver_id: char_id,
            processed: false,
            created: (11_644_473_600 + 1_700_000_000) * 10_000_000,
            data: "attack".into(),
            deleted: false,
            interacted: false,
        });
        recs.push(NotificationRecord {
            notification_id: 30,
            type_id: 5,
            group_id: 1,
            sender_id: corp_id,
            receiver_id: char_id,
            processed: false,
            created: 0,
            data: "deleted".into(),
            deleted: true,
            interacted: false,
        });
    }
    ctx.esi_state.contact_notifications.write().unwrap().insert(
        char_id,
        vec![EsiContactNotification {
            notification_id: 77,
            sender_character_id: c_female,
            send_date_unix: 1_700_000_000,
            standing_level: 5.0,
            message: "Added you".into(),
        }],
    );

    for ep in ["/characters/{character_id}/notifications", "/characters/{character_id}/notifications/contacts"] {
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &empty, &empty, b"").unwrap().status_code, 400);
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &params(&[("character_id", "99999999")]), &empty, b"").unwrap().status_code, 404);
    }
    let notifs = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/notifications", &p_char, &empty, b"").unwrap();
    let narr = body(&notifs).as_array().unwrap();
    assert_eq!(narr.len(), 2);
    assert_eq!(narr[0]["notification_id"], 20);
    assert_eq!(narr[0]["type"], "StructureUnderAttack");
    assert_eq!(narr[0]["sender_type"], "alliance");
    assert_eq!(narr[1]["timestamp"], "1970-01-01T00:00:00Z");

    let cnotifs = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/notifications/contacts", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&cnotifs)[0]["notification_id"], 77);
    assert_eq!(body(&cnotifs)[0]["message"], "Added you");
}

#[test]
fn research_medals_scoped_roles_titles_calendar_bookmarks_and_mail_delegation() {
    let (ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let now = ctx.now_unix();
    let empty = HashMap::new();
    let char_s = char_id.to_string();
    let corp_s = corp_id.to_string();
    let p_char = params(&[("character_id", &char_s)]);
    let p_corp = params(&[("corporation_id", &corp_s)]);

    let c_title_str = 90_810_001_i64;
    let c_no_title = 90_810_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&char_id].clone();
        let mut t1 = tpl.clone();
        t1.character_id = c_title_str;
        t1.title = " Chief Engineer ".trim().to_string();
        snap.characters.insert(c_title_str, t1);

        let mut t2 = tpl;
        t2.character_id = c_no_title;
        t2.title = String::new();
        snap.characters.insert(c_no_title, t2);

        snap.char_research_agents.insert(
            char_id,
            vec![
                CharacterResearchAgentRecord { agent_id: 3_010_002, skill_type_id: 11_448, started_at_unix: 1_700_000_000, points_per_day: 55.5, remainder_points: 120.0 },
                CharacterResearchAgentRecord { agent_id: 3_010_001, skill_type_id: 11_452, started_at_unix: 1_690_000_000, points_per_day: 42.0, remainder_points: 10.0 },
            ],
        );
        snap.char_medals.insert(
            char_id,
            vec![CharacterMedalRecord {
                medal_id: 5,
                title: "Star".into(),
                description: "Hero".into(),
                corporation_id: corp_id,
                issuer_id: char_id,
                date_unix: 1_700_000_000,
                reason: "Valor".into(),
                status: "public".into(),
                graphics: vec![CharacterMedalGraphicRecord { part: 1, layer: 2, graphic: "caldari.1_1".into(), color: 255 }],
            }],
        );
        snap.char_scoped_roles.insert(
            char_id,
            CharacterScopedRolesRecord {
                roles: vec!["Director".into()],
                roles_at_hq: vec!["Hangar_Take_1".into()],
                roles_at_base: vec!["Hangar_Query_1".into()],
                roles_at_other: vec!["Accountant".into()],
            },
        );

        // Titles via corp_titles, corp_governance.titles, and unknown fallback
        snap.corp_member_titles.insert((corp_id, char_id), vec![4, 2, 8]);
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry { title_id: 2, name: "Fleet Boss".into(), ..Default::default() }],
        );
        let mut gov = CorpGovState::default();
        gov.titles.insert(4, ("Quartermaster".into(), 0));
        snap.corp_governance.insert(corp_id, gov);

        // Bookmarks & Folders (char + corp, subfolders, expiries, shared/corp-folder rules)
        snap.bookmarks.clear();
        snap.bookmark_folders.clear();
        snap.bookmark_subfolder_parents.clear();
        snap.bookmark_expiries.clear();

        snap.bookmark_folders.insert(10, BookmarkFolderRecord { folder_id: 10, owner_id: char_id, folder_name: "Personal Spots".into(), is_shared: false, corporation_id: None, ..Default::default() });
        snap.bookmark_folders.insert(11, BookmarkFolderRecord { folder_id: 11, owner_id: char_id, folder_name: "Subfolder".into(), is_shared: false, corporation_id: None, ..Default::default() });
        snap.bookmark_subfolder_parents.insert(11, 10);
        snap.bookmark_folders.insert(12, BookmarkFolderRecord { folder_id: 12, owner_id: 90_000_002, folder_name: "Shared Lookup".into(), is_shared: true, corporation_id: Some(corp_id), ..Default::default() });

        // Char bookmarks: active with folder+item, active with exp == 0, active with exp == now + 10, expired with exp == now, and referencing unknown folder 99
        snap.bookmarks.insert(1, BookmarkRecord { bookmark_id: 1, owner_id: char_id, label: "BM1".into(), solar_system_id: 30_000_142, x: 1.0, y: 2.0, z: 3.0, item_id: Some(60_003_760), memo: "m1".into(), folder_id: Some(12), is_shared: false });
        snap.bookmarks.insert(2, BookmarkRecord { bookmark_id: 2, owner_id: char_id, label: "BM2".into(), solar_system_id: 30_000_142, x: 0.0, y: 0.0, z: 0.0, item_id: None, memo: String::new(), folder_id: Some(99), is_shared: true });
        snap.bookmark_expiries.insert(2, 0);
        snap.bookmarks.insert(3, BookmarkRecord { bookmark_id: 3, owner_id: char_id, label: "BM_Exp".into(), solar_system_id: 30_000_142, x: 0.0, y: 0.0, z: 0.0, item_id: None, memo: String::new(), folder_id: None, is_shared: true });
        snap.bookmark_expiries.insert(3, now);
        // Corp-owned bookmark referencing unknown folder 88 + item
        snap.bookmarks.insert(4, BookmarkRecord { bookmark_id: 4, owner_id: corp_id, label: "CorpBM".into(), solar_system_id: 30_000_142, x: 5.0, y: 6.0, z: 7.0, item_id: Some(60_003_760), memo: "corp".into(), folder_id: Some(88), is_shared: false });
        snap.bookmark_expiries.insert(4, now + 60);
    });

    for ep in [
        "/characters/{character_id}/agents_research",
        "/characters/{character_id}/medals",
        "/characters/{character_id}/roles",
        "/characters/{character_id}/titles",
        "/characters/{character_id}/bookmarks",
        "/characters/{character_id}/bookmarks/folders",
        "/characters/{character_id}/opportunities",
    ] {
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &empty, &empty, b"").unwrap().status_code, 400);
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &params(&[("character_id", "99999999")]), &empty, b"").unwrap().status_code, 404);
    }
    for ep in ["/corporations/{corporation_id}/bookmarks", "/corporations/{corporation_id}/bookmarks/folders"] {
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &empty, &empty, b"").unwrap().status_code, 400);
        assert_eq!(dispatch_char_social_route(&ctx, "GET", ep, &params(&[("corporation_id", "99999999")]), &empty, b"").unwrap().status_code, 404);
    }

    let res_ag = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/agents_research", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&res_ag)[0]["agent_id"], 3_010_001);
    assert_eq!(body(&res_ag)[1]["points_per_day"], 55.5);

    let med = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/medals", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&med)[0]["graphics"][0]["graphic"], "caldari.1_1");

    let roles = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/roles", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&roles)["roles_at_other"], json!(["Accountant"]));

    // Titles: corp_member_titles resolution, runtime char_titles override, ch.title fallback, and empty
    ctx.esi_state.char_titles.write().unwrap().clear();
    let t_corp = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/titles", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&t_corp), &json!([{ "title_id": 2, "name": "Fleet Boss" }, { "title_id": 4, "name": "Quartermaster" }, { "title_id": 8, "name": "Title 8" }]));

    ctx.esi_state.char_titles.write().unwrap().insert(char_id, vec![(1, "Runtime Title".into())]);
    let t_rt = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/titles", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&t_rt), &json!([{ "title_id": 1, "name": "Runtime Title" }]));

    let t_str = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/titles", &params(&[("character_id", &c_title_str.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&t_str), &json!([{ "title_id": 1, "name": "Chief Engineer" }]));

    let t_none = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/titles", &params(&[("character_id", &c_no_title.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&t_none), &json!([]));

    // Bookmarks & Folders
    let cbms = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/bookmarks", &p_char, &empty, b"").unwrap();
    let cbms_arr = body(&cbms).as_array().unwrap();
    assert_eq!(cbms_arr.len(), 2); // BM1 & BM2 (BM_Exp excluded at exp == now)
    assert_eq!(cbms_arr[0]["folder_id"], 12);
    assert_eq!(cbms_arr[0]["item"]["item_id"], 60_003_760);

    let cflds = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/bookmarks/folders", &p_char, &empty, b"").unwrap();
    let cfld_arr = body(&cflds).as_array().unwrap();
    assert_eq!(cfld_arr.len(), 3); // 10 ("Personal Spots"), 12 ("Shared Lookup"), 99 ("Folder 99")
    assert!(cfld_arr.iter().any(|f| f["name"] == "Shared Lookup") && cfld_arr.iter().any(|f| f["name"] == "Folder 99"));

    let corpbms = dispatch_char_social_route(&ctx, "GET", "/corporations/{corporation_id}/bookmarks", &p_corp, &empty, b"").unwrap();
    let corpbms_arr = body(&corpbms).as_array().unwrap();
    // Includes BM1 (in corp/shared folder 12), BM2 (member is_shared), CorpBM (owner == corp_id); excludes BM_Exp
    assert_eq!(corpbms_arr.len(), 3);

    let corpflds = dispatch_char_social_route(&ctx, "GET", "/corporations/{corporation_id}/bookmarks/folders", &p_corp, &empty, b"").unwrap();
    let corpfld_arr = body(&corpflds).as_array().unwrap();
    assert!(corpfld_arr.iter().any(|f| f["name"] == "Shared Lookup") && corpfld_arr.iter().any(|f| f["name"] == "Corp Folder 88"));

    // Calendar, Opportunities (groups, tasks, populated & empty), Mail delegation & unknown None
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/opportunities/groups", &empty, &empty, b"").unwrap().status_code, 200);
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/opportunities/tasks", &empty, &empty, b"").unwrap().status_code, 200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            char_id,
            eve_db::NpeProgressRecord {
                character_id: char_id,
                node_graph_id: 1,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: std::collections::HashSet::from([202, 101]),
            },
        );
    });
    let opp_pop = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/opportunities", &p_char, &empty, b"").unwrap();
    assert_eq!(body(&opp_pop)[0]["task_id"], 101);
    assert_eq!(body(&opp_pop)[1]["task_id"], 202);
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/calendar", &p_char, &empty, b"").unwrap().status_code, 200);
    let opp_empty = dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/opportunities", &params(&[("character_id", &c_no_title.to_string())]), &empty, b"").unwrap();
    assert_eq!(body(&opp_empty), &json!([]));
    assert_eq!(dispatch_char_social_route(&ctx, "GET", "/characters/{character_id}/mail/lists", &p_char, &empty, b"").unwrap().status_code, 200);
    assert!(dispatch_char_social_route(&ctx, "GET", "/unknown/route", &empty, &empty, b"").is_none());
}
