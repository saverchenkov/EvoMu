//! Characterization and caller tests for unified corporation role evaluation (`#1194`).

#![forbid(unsafe_code)]

use crate::domain::corp_governance::{
    can_send_corp_mail, corp_role_breakdown, decode_corp_roles, encode_esi_corp_roles,
    CorpRoleBreakdown, CORP_ROLE_DEFINITIONS,
};
use eve_db::{
    corp_roles, CorpMemberRoleAssignment, CorpTitleEntry, CorporationRecord, WorldStateSnapshot,
};
use std::collections::HashSet;

const CORP_ID: i64 = 98_220_001;
const CEO_ID: i64 = 92_200_001;
const MEM_ID: i64 = 92_200_002;
const HQ_ST: i64 = 60_003_760;
const BASE_ST: i64 = 60_003_761;
const OTHER_ST: i64 = 60_003_762;
const MATURED_AT: u64 = 1_800_086_400;

fn seed_corp(snap: &mut WorldStateSnapshot) {
    let mut corp = CorporationRecord::new(CORP_ID, "Role Char Corp", "RCC", CEO_ID);
    corp.hq_station_id = Some(HQ_ST);
    corp.members = HashSet::from([CEO_ID, MEM_ID]);
    snap.corporations.insert(CORP_ID, corp);
    snap.corp_governance
        .entry(CORP_ID)
        .or_default()
        .member_bases
        .insert(MEM_ID, BASE_ST);
}

#[derive(Clone, Copy)]
struct CharacterizationCase {
    name: &'static str,
    char_id: i64,
    now_unix: u64,
    setup: fn(&mut WorldStateSnapshot),
    expected_eval_global: (bool, u64),
    expected_eval_hq: (bool, u64),
    expected_eval_base: (bool, u64),
    expected_eval_other: (bool, u64),
    expected_bd: CorpRoleBreakdown,
}

fn bd_with(
    is_ceo: bool,
    roles: u64,
    roles_at_hq: u64,
    roles_at_base: u64,
    roles_at_other: u64,
    grantable_roles: u64,
    unmatured_mask: u64,
) -> CorpRoleBreakdown {
    CorpRoleBreakdown {
        is_ceo,
        roles,
        roles_at_hq,
        roles_at_base,
        roles_at_other,
        grantable_roles,
        grantable_roles_at_hq: 0,
        grantable_roles_at_base: 0,
        grantable_roles_at_other: 0,
        unmatured_mask,
    }
}

#[test]
fn corp_role_evaluator_vs_breakdown_characterization() {
    let cases = [
        CharacterizationCase {
            name: "missing_corp",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles: corp_roles::ROLE_ACCOUNTANT,
                        roles_at_hq: corp_roles::ROLE_TRADER,
                        ..Default::default()
                    },
                );
            },
            expected_eval_global: (false, 0),
            expected_eval_hq: (false, 0),
            expected_eval_base: (false, 0),
            expected_eval_other: (false, 0),
            expected_bd: CorpRoleBreakdown::default(),
        },
        CharacterizationCase {
            name: "plain_stored_roles",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corporations
                    .get_mut(&CORP_ID)
                    .unwrap()
                    .member_roles
                    .insert(MEM_ID, corp_roles::ROLE_AUDITOR);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles: corp_roles::ROLE_SECURITY_OFFICER,
                        grantable_roles: corp_roles::ROLE_PERSONNEL_MANAGER,
                        ..Default::default()
                    },
                );
            },
            expected_eval_global: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_SECURITY_OFFICER,
            ),
            expected_eval_hq: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_SECURITY_OFFICER,
            ),
            expected_eval_base: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_SECURITY_OFFICER,
            ),
            expected_eval_other: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_SECURITY_OFFICER,
            ),
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_SECURITY_OFFICER,
                0,
                0,
                0,
                corp_roles::ROLE_PERSONNEL_MANAGER,
                0,
            ),
        },
        CharacterizationCase {
            name: "hq_base_other_assignments",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles: corp_roles::ROLE_AUDITOR,
                        grantable_roles: corp_roles::ROLE_TRADER,
                        roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                        roles_at_base: corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                        roles_at_other: corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                        ..Default::default()
                    },
                );
            },
            expected_eval_global: (false, corp_roles::ROLE_AUDITOR),
            expected_eval_hq: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_HANGAR_CAN_TAKE[0],
            ),
            expected_eval_base: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
            ),
            expected_eval_other: (
                false,
                corp_roles::ROLE_AUDITOR | corp_roles::ROLE_HANGAR_CAN_TAKE[2],
            ),
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_AUDITOR,
                corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                corp_roles::ROLE_TRADER,
                0,
            ),
        },
        CharacterizationCase {
            name: "title_ids_1_2_4_8_gov_titles",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                let gov = snap.corp_governance.entry(CORP_ID).or_default();
                gov.titles
                    .insert(1, ("T1".into(), corp_roles::ROLE_SECURITY_OFFICER));
                gov.titles
                    .insert(2, ("T2".into(), corp_roles::ROLE_FACTORY_MANAGER));
                gov.titles
                    .insert(4, ("T4".into(), corp_roles::ROLE_STATION_MANAGER));
                gov.titles
                    .insert(8, ("T8".into(), corp_roles::ROLE_AUDITOR));
                gov.member_titles.insert(MEM_ID, 1 | 2 | 4 | 8);
            },
            expected_eval_global: (
                false,
                corp_roles::ROLE_SECURITY_OFFICER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_AUDITOR,
            ),
            expected_eval_hq: (
                false,
                corp_roles::ROLE_SECURITY_OFFICER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_AUDITOR,
            ),
            expected_eval_base: (
                false,
                corp_roles::ROLE_SECURITY_OFFICER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_AUDITOR,
            ),
            expected_eval_other: (
                false,
                corp_roles::ROLE_SECURITY_OFFICER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_AUDITOR,
            ),
            // Rule 1: title IDs map through title_id_to_bit (title IDs 4 and 8 now match evaluator).
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_SECURITY_OFFICER
                    | corp_roles::ROLE_FACTORY_MANAGER
                    | corp_roles::ROLE_STATION_MANAGER
                    | corp_roles::ROLE_AUDITOR,
                0,
                0,
                0,
                0,
                0,
            ),
        },
        CharacterizationCase {
            name: "pending_role_grant_delayed_and_immediate_before_maturity",
            char_id: MEM_ID,
            now_unix: MATURED_AT - 1,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles: corp_roles::ROLE_ACCOUNTANT,
                        ..Default::default()
                    },
                );
                snap.corp_governance
                    .entry(CORP_ID)
                    .or_default()
                    .pending_role_grants
                    .insert(
                        MEM_ID,
                        (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT, MATURED_AT),
                    );
            },
            expected_eval_global: (false, corp_roles::ROLE_ACCOUNTANT),
            expected_eval_hq: (false, corp_roles::ROLE_ACCOUNTANT),
            expected_eval_base: (false, corp_roles::ROLE_ACCOUNTANT),
            expected_eval_other: (false, corp_roles::ROLE_ACCOUNTANT),
            // Rule 2: immediate ROLE_ACCOUNTANT is no longer hidden before maturity; only DELAYED_STASIS_MASK is suppressed.
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_ACCOUNTANT,
                0,
                0,
                0,
                0,
                corp_roles::ROLE_DIRECTOR,
            ),
        },
        CharacterizationCase {
            name: "pending_role_grant_delayed_and_immediate_after_maturity",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles: corp_roles::ROLE_ACCOUNTANT,
                        ..Default::default()
                    },
                );
                snap.corp_governance
                    .entry(CORP_ID)
                    .or_default()
                    .pending_role_grants
                    .insert(
                        MEM_ID,
                        (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT, MATURED_AT),
                    );
            },
            expected_eval_global: (
                false,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
            ),
            expected_eval_hq: (
                false,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
            ),
            expected_eval_base: (
                false,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
            ),
            expected_eval_other: (
                false,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
            ),
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT,
                0,
                0,
                0,
                0,
                0,
            ),
        },
        CharacterizationCase {
            name: "pending_scoped_role_grant_before_maturity",
            char_id: MEM_ID,
            now_unix: MATURED_AT - 1,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                        ..Default::default()
                    },
                );
                snap.corp_governance
                    .entry(CORP_ID)
                    .or_default()
                    .pending_scoped_role_grants
                    .insert(
                        MEM_ID,
                        (
                            corp_roles::ROLE_HANGAR_CAN_TAKE[0]
                                | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                            corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                            corp_roles::ROLE_HANGAR_CAN_TAKE[3],
                            MATURED_AT,
                        ),
                    );
            },
            expected_eval_global: (false, 0),
            expected_eval_hq: (false, 0),
            expected_eval_base: (false, 0),
            expected_eval_other: (false, 0),
            // Rule 3: unmatured scoped pending grant is removed from its location mask.
            expected_bd: bd_with(false, 0, 0, 0, 0, 0, 0),
        },
        CharacterizationCase {
            name: "pending_scoped_role_grant_after_maturity",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, MEM_ID),
                    CorpMemberRoleAssignment {
                        character_id: MEM_ID,
                        roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                        ..Default::default()
                    },
                );
                snap.corp_governance
                    .entry(CORP_ID)
                    .or_default()
                    .pending_scoped_role_grants
                    .insert(
                        MEM_ID,
                        (
                            corp_roles::ROLE_HANGAR_CAN_TAKE[0]
                                | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                            corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                            corp_roles::ROLE_HANGAR_CAN_TAKE[3],
                            MATURED_AT,
                        ),
                    );
            },
            expected_eval_global: (false, 0),
            expected_eval_hq: (
                false,
                corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
            ),
            expected_eval_base: (false, corp_roles::ROLE_HANGAR_CAN_TAKE[2]),
            expected_eval_other: (false, corp_roles::ROLE_HANGAR_CAN_TAKE[3]),
            // Rule 3: matured scoped pending grants count in their breakdown location masks.
            expected_bd: bd_with(
                false,
                0,
                corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                corp_roles::ROLE_HANGAR_CAN_TAKE[3],
                0,
                0,
            ),
        },
        CharacterizationCase {
            name: "corp_titles_via_member_title_mask_only",
            char_id: MEM_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_titles.insert(
                    CORP_ID,
                    vec![CorpTitleEntry {
                        title_id: 4,
                        name: "Quartermaster".into(),
                        roles: vec!["Trader".into()],
                        roles_at_hq: vec!["Diplomat".into()],
                        grantable_roles: vec!["Auditor".into()],
                        ..Default::default()
                    }],
                );
                snap.corp_governance
                    .entry(CORP_ID)
                    .or_default()
                    .member_titles
                    .insert(MEM_ID, 4);
            },
            expected_eval_global: (false, corp_roles::ROLE_TRADER),
            expected_eval_hq: (false, corp_roles::ROLE_TRADER | corp_roles::ROLE_DIPLOMAT),
            expected_eval_base: (false, corp_roles::ROLE_TRADER),
            expected_eval_other: (false, corp_roles::ROLE_TRADER),
            // Rule 4: a title held only through member's title_mask counts in the breakdown.
            expected_bd: bd_with(
                false,
                corp_roles::ROLE_TRADER,
                corp_roles::ROLE_DIPLOMAT,
                0,
                0,
                corp_roles::ROLE_AUDITOR,
                0,
            ),
        },
        CharacterizationCase {
            name: "ceo_contract",
            char_id: CEO_ID,
            now_unix: MATURED_AT,
            setup: |snap| {
                seed_corp(snap);
                snap.corp_role_assignments.insert(
                    (CORP_ID, CEO_ID),
                    CorpMemberRoleAssignment {
                        character_id: CEO_ID,
                        roles: corp_roles::ROLE_ACCOUNTANT,
                        grantable_roles: corp_roles::ROLE_DIRECTOR,
                        ..Default::default()
                    },
                );
            },
            expected_eval_global: (true, u64::MAX),
            expected_eval_hq: (true, u64::MAX),
            expected_eval_base: (true, u64::MAX),
            expected_eval_other: (true, u64::MAX),
            expected_bd: bd_with(
                true,
                corp_roles::ROLE_ACCOUNTANT,
                0,
                0,
                0,
                corp_roles::ROLE_DIRECTOR,
                0,
            ),
        },
    ];

    for case in cases {
        let mut snap = WorldStateSnapshot::default();
        (case.setup)(&mut snap);

        assert_eq!(
            snap.effective_corp_roles_at_db(CORP_ID, case.char_id, None, case.now_unix),
            case.expected_eval_global,
            "{}: eval global",
            case.name
        );
        assert_eq!(
            snap.effective_corp_roles_at_db(CORP_ID, case.char_id, Some(HQ_ST), case.now_unix),
            case.expected_eval_hq,
            "{}: eval hq",
            case.name
        );
        assert_eq!(
            snap.effective_corp_roles_at_db(CORP_ID, case.char_id, Some(BASE_ST), case.now_unix),
            case.expected_eval_base,
            "{}: eval base",
            case.name
        );
        assert_eq!(
            snap.effective_corp_roles_at_db(CORP_ID, case.char_id, Some(OTHER_ST), case.now_unix),
            case.expected_eval_other,
            "{}: eval other",
            case.name
        );
        assert_eq!(
            corp_role_breakdown(&snap, CORP_ID, case.char_id, case.now_unix),
            case.expected_bd,
            "{}: breakdown",
            case.name
        );
    }
}

#[test]
fn corp_role_definitions_matches_corp_role_names_and_encoders() {
    assert_eq!(CORP_ROLE_DEFINITIONS.len(), corp_roles::CORP_ROLE_NAMES.len());
    assert_eq!(CORP_ROLE_DEFINITIONS, corp_roles::CORP_ROLE_NAMES);
    assert_eq!(encode_esi_corp_roles(&[]), 0);
    assert_eq!(corp_roles::encode_corp_role_names(&[]), 0);

    for &(bit, client_name, esi_name) in CORP_ROLE_DEFINITIONS {
        let client_inputs = [
            client_name.to_string(),
            client_name.to_ascii_lowercase(),
            client_name.to_ascii_uppercase(),
        ];
        assert_eq!(encode_esi_corp_roles(&client_inputs), bit);
        assert_eq!(corp_roles::encode_corp_role_names(&client_inputs), bit);

        if !esi_name.is_empty() {
            let esi_inputs = [
                esi_name.to_string(),
                esi_name.to_ascii_lowercase(),
                esi_name.to_ascii_uppercase(),
            ];
            assert_eq!(encode_esi_corp_roles(&esi_inputs), bit);
            assert_eq!(corp_roles::encode_corp_role_names(&esi_inputs), bit);
        }
    }

    let empty_and_unknown = vec![String::new(), "UnknownCorpRole".to_string()];
    assert_eq!(encode_esi_corp_roles(&empty_and_unknown), 0);
    assert_eq!(corp_roles::encode_corp_role_names(&empty_and_unknown), 0);
    assert_eq!(decode_corp_roles(0), Vec::<&str>::new());
}

#[test]
fn can_send_corp_mail_honors_scoped_pending_grant_maturity() {
    let mut snap = WorldStateSnapshot::default();
    seed_corp(&mut snap);
    snap.corp_role_assignments.insert(
        (CORP_ID, MEM_ID),
        CorpMemberRoleAssignment {
            character_id: MEM_ID,
            roles_at_hq: corp_roles::ROLE_CHAT_MANAGER,
            ..Default::default()
        },
    );
    let gov = snap.corp_governance.entry(CORP_ID).or_default();
    gov.restrict_corp_mails = true;
    gov.pending_scoped_role_grants.insert(
        MEM_ID,
        (
            corp_roles::ROLE_CHAT_MANAGER,
            corp_roles::ROLE_DIPLOMAT,
            0,
            MATURED_AT,
        ),
    );

    assert!(!can_send_corp_mail(&snap, CORP_ID, MEM_ID, MATURED_AT - 1));
    assert!(can_send_corp_mail(&snap, CORP_ID, MEM_ID, MATURED_AT));
}
