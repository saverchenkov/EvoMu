//! Additional unit and mutation-killing tests for `quasar_corp_domain`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use crate::services::quasar_corp_domain::register;
use eve_db::{
    corp_roles, AllianceLifecycleMeta, AllianceRecord, CharacterLoginStats,
    CorpAllianceHistoryEntry, CorpContainerLogEntry, CorpGovState, CorpIssuedMedalEntry,
    CorpMedalEntry, CorpMemberRoleAssignment, CorpRoleHistoryEntry, CorpTitleEntry,
    CorpVoteCaseRecord, CorpWalletDivisionRecord, CorporationRecord, CustomsOfficeRecord,
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, ObserverMiningRecord,
    SocialContactRecord, StarbaseDetailSettings, UpwellStructureRecord, WalletTransactionRecord,
    WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::{InvType, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        sde.types.insert(687, InvType {
            type_id: 687, group_id: 105, category_id: 9, name: "Rifter Blueprint".into(),
            mass: 1.0, volume: 0.01, capacity: 0.0, radius: 1.0, base_price: 1000.0,
            published: true, portion_size: 1, market_group_id: None, race_id: None,
            faction_id: None, variation_parent_type_id: None, description_id: None,
            icon_id: None, graphic_id: None, sound_id: None, meta_group_id: None,
            meta_level: None, tech_level: None, default_effect_id: None,
            description: String::new(), effect_ids: Vec::new(), default_attributes: HashMap::new(),
        });
        Arc::new(sde)
    }))
}

fn seq_id_msg(field: u32, id: u32) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("SequentialId");
    inner.fields.insert(1, DynamicValue::Uint32(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn quasar_alliance_corporation_handlers_deep_data_branches_and_edge_cases() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 980_101_i64;
    let cid2 = 980_102_i64;
    let corp_id = 98_800_101_i64;
    let corp_no_ally = 98_800_102_i64;
    let npc_corp_id = 1_000_009_i64;
    let ally_id = 99_800_101_i64;

    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        let mut ch1 = base_ch.clone();
        ch1.character_id = cid;
        ch1.corporation_id = corp_id;
        ch1.alliance_id = Some(ally_id);
        ch1.station_id = Some(60_003_760);
        ch1.active_ship_id = 700_001;
        ch1.create_date_unix = 1_700_000_000;
        snap.characters.insert(cid, ch1);

        let mut ch2 = base_ch;
        ch2.character_id = cid2;
        ch2.corporation_id = corp_id;
        ch2.alliance_id = Some(ally_id);
        ch2.structure_id = Some(1_040_001_001);
        snap.characters.insert(cid2, ch2);

        snap.character_login_stats.insert(cid, CharacterLoginStats {
            last_login_unix: 1_750_000_000, last_logout_unix: 1_750_003_600, logins_count: 42,
        });

        let mut c = CorporationRecord {
            corporation_id: corp_id, ceo_id: cid, hq_station_id: Some(60_003_760),
            alliance_id: Some(ally_id), name: "DeepQuasarCorp".into(), ticker: "DQSC".into(),
            master_wallet: IskCents::from_isk(5_000_000),
            wallet_divisions: vec![
                CorpWalletDivisionRecord { division_id: 1, name: "Primary Treasury".into(), balance: IskCents::from_isk(5_000_000) },
                CorpWalletDivisionRecord { division_id: 2, name: "Deficit Ops".into(), balance: IskCents(-25_000) },
            ],
            ..Default::default()
        };
        c.members.extend([cid, cid2]);
        c.member_roles.insert(cid2, corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_TRADER);
        snap.corporations.insert(corp_id, c);

        snap.corporations.insert(corp_no_ally, CorporationRecord {
            corporation_id: corp_no_ally, ceo_id: cid, name: "StandaloneCorp".into(), ticker: "STND".into(), ..Default::default()
        });
        snap.corporations.insert(npc_corp_id, CorporationRecord {
            corporation_id: npc_corp_id, ceo_id: 3_000_001, name: "Caldari Provisions".into(), ticker: "CP".into(), ..Default::default()
        });

        snap.alliances.insert(ally_id, AllianceRecord {
            alliance_id: ally_id, name: "DeepQuasarAlliance".into(), short_name: "DQSA".into(),
            executor_corp_id: corp_id, member_corp_ids: HashSet::from([corp_id]),
        });
        snap.alliance_lifecycle_meta.insert(ally_id, AllianceLifecycleMeta {
            creator_id: cid, creator_corporation_id: corp_id, date_founded_unix: 1_710_000_000, faction_id: Some(500_001),
        });

        snap.corp_hangar_divisions.insert(corp_id, HashMap::from([(1, "Ore Bay".into()), (4, "Blueprints Vault".into())]));
        snap.corp_alliance_history.insert(corp_id, vec![
            CorpAllianceHistoryEntry { record_id: 1, alliance_id: Some(ally_id), start_date_unix: 1_710_000_000, is_deleted: Some(true) },
            CorpAllianceHistoryEntry { record_id: 2, alliance_id: None, start_date_unix: 1_700_000_000, is_deleted: None },
        ]);

        let actions_and_flags = [
            ("assemble", "Hangar"), ("repackage", "Cargo"), ("set_name", "CorpSAG1"),
            ("move", "CorpSAG2"), ("set_password", "CorpSAG3"), ("add", "CorpSAG4"),
            ("lock", "CorpSAG5"), ("unlock", "CorpSAG6"), ("enter_password", "CorpSAG7"),
            ("configure", "999"), ("custom_action", "UnknownFlag"),
        ];
        snap.corp_container_logs.insert(
            corp_id,
            actions_and_flags.iter().enumerate().map(|(i, (act, flg))| CorpContainerLogEntry {
                logged_at_unix: 1_750_000_000 + i as u64, container_id: 600_000 + i as i64,
                container_type_id: 3293, character_id: cid, location_id: 60_003_760,
                location_flag: (*flg).to_string(), action: (*act).to_string(),
                password_type: Some("config".into()), type_id: Some(34), quantity: Some(100),
                old_config_bitmask: Some(1), new_config_bitmask: Some(3),
            }).collect(),
        );

        for (iid, tid, loc, flag, qty, sing, bpc, runs, attrs) in [
            (720_001, 687, 60_003_760, InventoryFlag::CorpSAG1, 3, false, false, None, HashMap::new()),
            (720_002, 687, 60_003_760, InventoryFlag::CorpSAG2, 1, true, true, Some(15), HashMap::from([(901, 10.0), (902, 20.0)])),
            (720_003, 687, 60_003_760, InventoryFlag::CorpSAG3, 1, true, false, None, HashMap::from([(901, 8.0), (902, 16.0)])),
            (720_004, 27, 60_003_760, InventoryFlag::Hangar, 1, true, false, None, HashMap::new()),
            (720_005, 4051, 1_040_001_001, InventoryFlag::StructureFuel, 400, false, false, None, HashMap::new()),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: corp_id, location_id: loc, flag,
                quantity: qty, singleton: sing, is_blueprint_copy: bpc,
                blueprint_runs_remaining: runs, dynamic_attributes: attrs, ..Default::default()
            });
        }

        for (idx, act_id) in [1_u8, 3, 4, 5, 8, 9, 11].into_iter().enumerate() {
            let jid = 810_000 + idx as i64;
            snap.industry_jobs.insert(jid, IndustryJobRecord {
                job_id: jid, installer_id: cid, facility_id: 60_003_760, blueprint_item_id: 720_001,
                blueprint_type_id: 687, activity_id: act_id, runs: 5, product_type_id: 587,
                status: "Active".into(), duration_seconds: 3_600, start_unix: 1_750_000_000,
                blueprint_location_id: 60_003_760, output_location_id: 60_003_760,
                cost: Some(12_500.0), licensed_runs: Some(10), probability: Some(0.45),
                completed_unix: None, completed_character_id: None,
            });
        }

        for (idx, st_state) in ["Unanchoring", "Offline", "Anchoring", "ArmorReinforced", "ShieldVulnerable"].into_iter().enumerate() {
            let sid = 1_040_001_001 + idx as i64;
            snap.structures.insert(sid, UpwellStructureRecord {
                structure_id: sid, type_id: 35835, owner_corp_id: corp_id, solar_system_id: 30_000_142,
                name: format!("Corp Structure {idx}"), position: [0.0; 3], state: st_state.into(),
                quantum_core_installed: true, fuel_blocks: 100, reinforced_until_unix: 1_760_000_000,
                self_destruct_at_unix: Some(1_770_000_000), ..Default::default()
            });
        }

        snap.observer_mining_ledger.insert(1_040_001_001, vec![ObserverMiningRecord {
            observer_id: 1_040_001_001, character_id: cid, recorded_corporation_id: corp_id,
            type_id: 1230, quantity: 5_000, last_updated_date: "2026-01-01".into(), timestamp_unix: 1_750_000_000,
        }]);

        snap.corp_role_assignments.insert((corp_id, cid), CorpMemberRoleAssignment {
            character_id: cid, roles: corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
            grantable_roles: corp_roles::ROLE_TRADER, roles_at_hq: corp_roles::ROLE_STATION_MANAGER,
            grantable_roles_at_hq: corp_roles::ROLE_FACTORY_MANAGER, roles_at_base: corp_roles::ROLE_AUDITOR,
            grantable_roles_at_base: corp_roles::ROLE_SECURITY_OFFICER, roles_at_other: corp_roles::ROLE_DIPLOMAT,
            grantable_roles_at_other: corp_roles::ROLE_PERSONNEL_MANAGER,
        });

        snap.corp_member_titles.insert((corp_id, cid), vec![1, 2]);
        let mut gov = CorpGovState::default();
        gov.member_limit = Some(125);
        gov.member_titles.insert(cid2, 0b101);
        gov.logo = (420, 421, 680, 681);
        snap.corp_governance.insert(corp_id, gov);

        let all_role_names: Vec<String> = [
            "Director", "Personnel_Manager", "Accountant", "Security_Officer", "Factory_Manager",
            "Station_Manager", "Auditor", "Diplomat", "Brand_Manager", "Starbase_Operator",
            "Config_Equipment", "Junior_Accountant", "Config_Starbase_Equipment", "Trader",
            "Chat_Manager", "Contract_Manager", "Infrastructure_Tactical_Officer",
            "Starbase_Defense_Operator", "Fitting_Manager", "Project_Manager",
            "Skill_Plan_Manager", "Deliveries_Hangar_Take", "Deliveries_Hangar_Query",
            "Deliveries_Container_Take", "Hangar_Take_1", "Hangar_Take_2", "Hangar_Take_3",
            "Hangar_Take_4", "Hangar_Take_5", "Hangar_Take_6", "Hangar_Take_7", "Hangar_Query_1",
            "Hangar_Query_2", "Hangar_Query_3", "Hangar_Query_4", "Hangar_Query_5",
            "Hangar_Query_6", "Hangar_Query_7", "Account_Take_1", "Account_Take_2",
            "Account_Take_3", "Account_Take_4", "Account_Take_5", "Account_Take_6",
            "Account_Take_7", "Container_Take_1", "Container_Take_2", "Container_Take_3",
            "Container_Take_4", "Container_Take_5", "Container_Take_6", "Container_Take_7",
            "Rent_Office", "Rent_Factory_Facility", "Rent_Research_Facility",
        ].into_iter().map(String::from).collect();

        let role_types = [
            "roles", "grantable_roles", "roles_at_hq", "grantable_roles_at_hq",
            "roles_at_base", "grantable_roles_at_base", "roles_at_other", "grantable_roles_at_other",
        ];
        snap.corp_role_history.insert(corp_id, role_types.iter().enumerate().map(|(i, rt)| CorpRoleHistoryEntry {
            character_id: cid, changed_at_unix: 1_750_000_000 + i as u64, issuer_id: cid,
            role_type: (*rt).into(), old_roles: vec!["Trader".into()], new_roles: all_role_names.clone(),
        }).collect());

        snap.corp_titles.insert(corp_id, vec![CorpTitleEntry {
            title_id: 1, name: "Chief Industrialist".into(), roles: all_role_names,
            grantable_roles: vec!["Trader".into()], roles_at_hq: vec!["Station_Manager".into()],
            grantable_roles_at_hq: vec![], roles_at_base: vec![], grantable_roles_at_base: vec![],
            roles_at_other: vec![], grantable_roles_at_other: vec![],
        }]);

        snap.corp_vote_cases.insert(901, CorpVoteCaseRecord {
            case_id: 901, corporation_id: corp_id, vote_type: 4, text: "Expand Hangar".into(),
            description: "Proposal to expand hangar".into(), parameter: 1,
            start_ts: 1_750_000_000, end_ts: 1_750_086_400, executed: false,
            options: vec![(1, "Yes".into(), 100), (2, "No".into(), 0)],
        });

        snap.starbase_settings.insert(1_040_001_001, StarbaseDetailSettings {
            allow_corporation_members: true, allow_alliance_members: true, use_alliance_standings: true,
            attack_if_other_security_status_dropping: true, attack_if_at_war: true,
            fuel_bay_view: "config_starbase_equipment".into(), fuel_bay_take: "starbase_fuel_technician".into(),
            anchor: "alliance_member".into(), unanchor: "corporation_member".into(),
            online: "config_starbase_equipment".into(), offline: "other_role".into(),
            attack_standing_threshold: Some(5.0), attack_security_status_threshold: Some(-2.5),
            ..Default::default()
        });

        snap.corp_medals.insert(corp_id, vec![CorpMedalEntry {
            medal_id: 501, title: "Star of Valor".into(), description: "Awarded for heroism".into(),
            creator_id: cid, created_at_unix: 1_750_000_000,
        }]);
        snap.corp_issued_medals.insert(corp_id, vec![
            CorpIssuedMedalEntry { medal_id: 501, character_id: cid, reason: "Defense of HQ".into(), status: "public".into(), issuer_id: cid, issued_at_unix: 1_750_001_000 },
            CorpIssuedMedalEntry { medal_id: 501, character_id: cid2, reason: "Classified Ops".into(), status: "private".into(), issuer_id: cid, issued_at_unix: 1_750_002_000 },
        ]);

        for div in [1_i64, 2, 3, 4] {
            snap.corp_wallet_transactions.insert((corp_id, div), vec![WalletTransactionRecord {
                transaction_id: 950_000 + div, date_unix: 1_750_000_000, location_id: 60_003_760,
                type_id: 34, unit_price: 12.5, quantity: 1_000, client_id: cid,
                is_buy: div % 2 == 0, is_personal: false, journal_ref_id: 960_000 + div,
            }]);
        }

        snap.corp_contacts.insert(corp_id, vec![
            SocialContactRecord { contact_id: 500_001, contact_type: "faction".into(), standing: 2.5, is_watched: false, is_blocked: false, label_ids: vec![] },
            SocialContactRecord { contact_id: 1_000_125, contact_type: "corporation".into(), standing: 4.0, is_watched: false, is_blocked: false, label_ids: vec![] },
            SocialContactRecord { contact_id: 3_019_001, contact_type: "agent".into(), standing: -1.5, is_watched: false, is_blocked: false, label_ids: vec![] },
            SocialContactRecord { contact_id: 42, contact_type: "other".into(), standing: 0.5, is_watched: false, is_blocked: false, label_ids: vec![] },
        ]);

        snap.corp_customs_offices.insert(corp_id, vec![CustomsOfficeRecord {
            office_id: 1_050_001, system_id: 30_000_142, reinforce_exit_start: 18, reinforce_exit_end: 20,
            allow_alliance_access: true, allow_access_with_standings: true, standing_level: Some("good".into()),
            corporation_tax_rate: Some(0.05), alliance_tax_rate: Some(0.07),
            excellent_standing_tax_rate: Some(0.08), good_standing_tax_rate: Some(0.10),
            neutral_standing_tax_rate: Some(0.12), bad_standing_tax_rate: Some(0.15),
            terrible_standing_tax_rate: Some(0.20),
        }]);
    });

    let auth_ctx = AuthoritativeContext::for_session(
        Some(1), Some(cid as u32), Some(corp_id as u32), corp_roles::ROLE_DIRECTOR, "tranquility", vec![1],
    );
    let auth_no_ally = AuthoritativeContext::for_session(
        Some(1), None, Some(corp_id as u32), corp_roles::ROLE_DIRECTOR, "tranquility", vec![1],
    );
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    assert_eq!(handlers["eve.alliance.GetRequest"](&auth_no_ally, &dummy_any, &DynamicQuasarMessage::new("Req")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert!(!handlers["eve.alliance.GetRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, ally_id as u32)).unwrap().value.is_empty());
    assert!(!handlers["eve.alliance.SearchRequest"](&auth_ctx, &dummy_any, &DynamicQuasarMessage::new("Search")).unwrap().value.is_empty());
    assert!(!handlers["eve.alliance.corporation.GetMembersRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, ally_id as u32)).unwrap().value.is_empty());

    assert_eq!(handlers["eve.corporation.npc.IsNPCCorporationRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, npc_corp_id as u32)).unwrap().value, vec![0x08, 0x01]);
    assert_eq!(handlers["eve.corporation.npc.IsNPCCorporationRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, corp_id as u32)).unwrap().value, vec![0x08, 0x00]);

    assert!(!handlers["eve.corporation.logo.api.GetRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, corp_id as u32)).unwrap().value.is_empty());
    assert_eq!(handlers["eve.corporation.logo.api.GetRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, 98_999_999)).unwrap_err().0, STATUS_NOT_FOUND);

    assert!(!handlers["eve.corporation.vote.GetRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, 901)).unwrap().value.is_empty());
    assert!(!handlers["eve.corporation.medals.medals.GetRequest"](&auth_ctx, &dummy_any, &seq_id_msg(1, corp_id as u32)).unwrap().value.is_empty());

    let mut corp_msg = seq_id_msg(1, corp_id as u32);
    let mut sb_id_inner = DynamicQuasarMessage::new("SequentialIdentifier");
    sb_id_inner.fields.insert(1, DynamicValue::Uint64(1_040_001_001));
    corp_msg.fields.insert(3, DynamicValue::Message(Box::new(sb_id_inner)));

    for req_name in [
        "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
        "eve.corporation.divisions.GetRequest",
        "eve.corporation.homestation.GetRequest",
        "eve.corporation.alliance.api.GetCurrentMembershipRequest",
        "eve.corporation.alliancemembershiprecord.GetAllRequest",
        "eve.corporation.alsclogs.GetRequest",
        "eve.corporation.industry.job.GetActiveRequest",
        "eve.corporation.industry.blueprints.GetRequest",
        "eve.corporation.industry.facilties.GetRequest",
        "eve.corporation.industry.observer.GetAllDetailsRequest",
        "eve.corporation.members.GetRequest",
        "eve.corporation.members.GetLimitRequest",
        "eve.corporation.members.roles.GetRequest",
        "eve.corporation.members.titles.GetRequest",
        "eve.corporation.members.tracking.GetRequest",
        "eve.corporation.members.roles.auditlog.GetRequest",
        "eve.corporation.npc.GetAllRequest",
        "eve.corporation.office.api.GetSolarSystemsRequest",
        "eve.corporation.structure.GetAllWithDetailsRequest",
        "eve.corporation.title.GetAllRequest",
        "eve.corporation.starbase.GetAllRequest",
        "eve.corporation.starbase.GetSettingsAndFuelsRequest",
        "eve.corporation.medals.GetRequest",
        "eve.corporation.medals.issued.GetRequest",
        "eve.corporation.market.entry.GetEntriesRequest",
        "eve.corporation.standing.npc.GetAllRequest",
        "eve.corporation.planetinteraction.office.GetRequest",
    ] {
        let resp = handlers[req_name](&auth_ctx, &dummy_any, &corp_msg)
            .unwrap_or_else(|e| panic!("{req_name} failed: {e:?}"));
        assert!(!resp.value.is_empty(), "{req_name} returned empty payload");
    }

    for div_val in [DynamicValue::Enum(1001), DynamicValue::Uint32(1002), DynamicValue::Uint64(1003)] {
        let mut m = seq_id_msg(1, corp_id as u32);
        m.fields.insert(2, div_val);
        assert!(!handlers["eve.corporation.market.entry.GetEntriesRequest"](&auth_ctx, &dummy_any, &m).unwrap().value.is_empty());
    }

    let auth_no_ally = AuthoritativeContext::for_session(
        Some(1), Some(cid as u32), Some(corp_no_ally as u32), corp_roles::ROLE_DIRECTOR, "tranquility", vec![1],
    );
    assert_eq!(handlers["eve.corporation.alliance.api.GetCurrentMembershipRequest"](&auth_no_ally, &dummy_any, &seq_id_msg(1, corp_no_ally as u32)).unwrap().value, vec![0x10, 0x01]);

    let unauth = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    assert_eq!(handlers["eve.corporation.alsclogs.GetRequest"](&unauth, &dummy_any, &corp_msg).unwrap_err().0, STATUS_FORBIDDEN);
}
