//! Standalone unit tests for `crate::domain::*` (`WS-D` / `D1..D5`).
//!
//! Covers `DomainError`, `career_goals`, `corp_governance`, `mail`, `market_acl`,
//! and `fleets` directly against the domain module without going through RPC/ESI
//! parity dispatch.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    AllianceRecord, CharacterRecord, ContractRecord, CorpMemberRoleAssignment, CorpTitleEntry,
    CorporationRecord, FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord,
    FleetWingRecord, InventoryFlag, InventoryItemRecord, IskCents, MailRecord, MailingListRecord,
    NeuralAttributes, NpeProgressRecord, SocialContactRecord, TrainedSkillRecord,
    UpwellStructureRecord, WarRecord, WorldRepository, WorldStateSnapshot,
};
use eve_sde::{CareerGoalTemplate, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_test_ctx() -> ServerContext {
    static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_char(
    char_id: i64,
    user_id: u64,
    corp_id: i64,
    alliance_id: Option<i64>,
    balance_isk: i64,
) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id: char_id,
        user_id,
        name: format!("Pilot {char_id}"),
        corporation_id: corp_id,
        alliance_id,
        war_faction_id: None,
        station_id: Some(60_003_760),
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 0,
        balance: IskCents::from_isk(balance_isk),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: NeuralAttributes {
            charisma: 20,
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            bonus_remaps: 2,
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
        },
        skill_points: 50_000.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 1_700_000_000,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn make_corp(corp_id: i64, ceo_id: i64, alliance_id: Option<i64>, members: &[i64]) -> CorporationRecord {
    let mut corp = CorporationRecord::new(corp_id, format!("Corp {corp_id}"), format!("C{corp_id}"), ceo_id);
    corp.alliance_id = alliance_id;
    corp.members = members.iter().copied().collect();
    corp
}

fn seed_char(snap: &mut WorldStateSnapshot, ch: CharacterRecord) {
    snap.characters.insert(ch.character_id, ch);
}

// ---------------------------------------------------------------------------
// D1: DomainError & career_goals
// ---------------------------------------------------------------------------
mod domain_error_and_career_goals_tests {
    use super::*;
    use crate::domain::career_goals::{
        claim_career_goal_or_milestone_in_snap, credit_character_reward_in_snap,
        resolve_career_goal_or_milestone,
    };
    use crate::services::quasar_career_goals::CareerMilestoneDef;

    const EVERMARKS_CORP_ID: i64 = 1_000_419;

    #[test]
    fn domain_error_constructors_set_expected_kind_code_and_default_http_status() {
        let cases = [
            (
                DomainError::invalid_input("BadArg", "bad arg msg"),
                DomainErrorKind::InvalidInput,
                400,
                "BadArg",
                "bad arg msg",
            ),
            (
                DomainError::forbidden("NoAccess", "access denied"),
                DomainErrorKind::Forbidden,
                403,
                "NoAccess",
                "access denied",
            ),
            (
                DomainError::not_found("MissingThing", "not found"),
                DomainErrorKind::NotFound,
                404,
                "MissingThing",
                "not found",
            ),
            (
                DomainError::conflict("AlreadyExists", "conflict"),
                DomainErrorKind::Conflict,
                409,
                "AlreadyExists",
                "conflict",
            ),
            (
                DomainError::precondition_failed("Precond", "precondition"),
                DomainErrorKind::PreconditionFailed,
                420,
                "Precond",
                "precondition",
            ),
            (
                DomainError::internal("InternalBoom", "boom"),
                DomainErrorKind::Internal,
                500,
                "InternalBoom",
                "boom",
            ),
        ];

        for (err, expected_kind, expected_status, expected_code, expected_msg) in cases {
            assert_eq!(err.kind, expected_kind);
            assert_eq!(err.http_status(), expected_status);
            assert_eq!(err.code, expected_code);
            assert_eq!(err.message, expected_msg);
            let display = format!("{err:?}");
            assert!(display.contains(expected_code));
            assert!(display.contains(expected_msg));
        }
    }

    #[test]
    fn domain_error_with_http_status_overrides_default_and_from_wal_error_maps_to_persistence_failure() {
        let overridden = DomainError::not_found("UserError", "missing list").with_http_status(400);
        assert_eq!(overridden.kind, DomainErrorKind::NotFound);
        assert_eq!(overridden.code, "UserError");
        assert_eq!(overridden.http_status(), 400);

        let wal_err: DomainError = eve_db::FromWalError::from_wal_error("disk full".to_string());
        assert_eq!(wal_err.kind, DomainErrorKind::Internal);
        assert_eq!(wal_err.code, "PersistenceFailure");
        assert_eq!(wal_err.message, "disk full");
        assert_eq!(wal_err.http_status(), 500);
    }

    #[test]
    fn credit_character_reward_rejects_missing_character_and_balance_overflow() {
        let mut snap = WorldStateSnapshot::default();
        let err_missing = credit_character_reward_in_snap(
            &mut snap,
            999_888_777,
            IskCents::from_isk(100),
            1000.0,
            10,
            None,
        )
        .unwrap_err();
        assert_eq!(err_missing.kind, DomainErrorKind::NotFound);
        assert_eq!(err_missing.code, "CharacterNotFound");

        let cid = 91_000_001;
        let mut ch = make_char(cid, 1, 98_000_001, None, 0);
        ch.balance = IskCents(i64::MAX);
        ch.free_skill_points = 500.0;
        seed_char(&mut snap, ch);

        let err_overflow = credit_character_reward_in_snap(
            &mut snap,
            cid,
            IskCents(1),
            250.0,
            5,
            Some(("career_goal", "overflow".to_string(), None)),
        )
        .unwrap_err();
        assert_eq!(err_overflow.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_overflow.code, "BalanceOverflow");
        assert_eq!(snap.characters[&cid].balance, IskCents(i64::MAX));
        assert!(snap.wallet_journal.is_empty());
    }

    #[test]
    fn credit_character_reward_updates_isk_sp_evermarks_and_respects_journal_meta_modes() {
        let mut snap = WorldStateSnapshot::default();
        let cid = 91_000_002;
        let corp_id = 98_000_010;
        seed_char(&mut snap, make_char(cid, 2, corp_id, None, 1_000));

        // 1. journal_meta = None: no wallet journal / ledger entry written.
        let (bal0, sp0) = credit_character_reward_in_snap(
            &mut snap,
            cid,
            IskCents::from_isk(0),
            0.0,
            0,
            None,
        )
        .unwrap();
        assert_eq!(bal0, IskCents::from_isk(1_000));
        assert_eq!(sp0, 0.0);
        assert!(snap.wallet_journal.is_empty());
        assert!(snap.wallet_journal_meta.is_empty());
        assert!(!snap.characters[&cid].loyalty_points.contains_key(&EVERMARKS_CORP_ID));

        // 2. journal_meta = Some((.., None)): legacy wallet_journal entry without ledger metadata.
        let (bal1, sp1) = credit_character_reward_in_snap(
            &mut snap,
            cid,
            IskCents::from_isk(250),
            1_500.0,
            40,
            Some(("career_goal_legacy", "legacy goal reward".to_string(), None)),
        )
        .unwrap();
        assert_eq!(bal1, IskCents::from_isk(1_250));
        assert_eq!(sp1, 1_500.0);
        assert_eq!(snap.characters[&cid].loyalty_points[&EVERMARKS_CORP_ID], 40);
        assert_eq!(snap.corp_lp_balances[&(corp_id, EVERMARKS_CORP_ID)], 40);
        assert_eq!(snap.wallet_journal.len(), 1);
        assert_eq!(snap.wallet_journal[0].entry_type, "career_goal_legacy");
        assert_eq!(snap.wallet_journal[0].reason, "legacy goal reward");
        assert_eq!(snap.wallet_journal[0].amount, IskCents::from_isk(250));
        assert_eq!(snap.wallet_journal[0].balance_after, IskCents::from_isk(1_250));
        assert!(snap.wallet_journal_meta.is_empty());

        // 3. journal_meta = Some((.., Some(ledger_extra))): full ledger entry + timestamp + cumulative evermarks.
        let ts = 1_712_345_678u64;
        let (bal2, sp2) = credit_character_reward_in_snap(
            &mut snap,
            cid,
            IskCents::from_isk(500),
            2_500.0,
            60,
            Some((
                "career_goal_reward",
                "full ledger reward".to_string(),
                Some((10, 4242, ts)),
            )),
        )
        .unwrap();
        assert_eq!(bal2, IskCents::from_isk(1_750));
        assert_eq!(sp2, 4_000.0);
        assert_eq!(snap.characters[&cid].loyalty_points[&EVERMARKS_CORP_ID], 100);
        assert_eq!(snap.corp_lp_balances[&(corp_id, EVERMARKS_CORP_ID)], 100);
        assert_eq!(snap.wallet_journal.len(), 2);
        let ledger_entry_id = snap.wallet_journal[1].entry_id;
        assert_eq!(snap.wallet_journal[1].entry_type, "career_goal_reward");
        assert_eq!(snap.wallet_journal_timestamps.get(&ledger_entry_id), Some(&ts));
        let meta = snap
            .wallet_journal_meta
            .get(&ledger_entry_id)
            .expect("ledger metadata recorded");
        assert_eq!(meta.ref_type_id, 10);
        assert_eq!(meta.first_party_id, 4242);
    }

    #[test]
    fn resolve_career_goal_or_milestone_prefers_milestone_then_sde_and_handles_missing() {
        let ctx = shared_test_ctx();
        let cid = 91_000_003i64;
        let mgr = &ctx.quasar_gateway.progression.career_goals;
        let goal_id = 77_001u32;

        // Build an SDE registry with a known career goal template.
        let mut sde = (*ctx.sde).clone();
        sde.career_goals.insert(
            goal_id,
            CareerGoalTemplate {
                goal_id,
                career_path: "Explorer".to_string(),
                name: "Scan Sites".to_string(),
                target_progress: 5,
                sp_reward: 1_250,
            },
        );

        // Unknown ID when not in milestones or SDE.
        assert!(resolve_career_goal_or_milestone(&sde, mgr, &[], cid, 999_999_999).is_none());

        let from_sde = resolve_career_goal_or_milestone(&sde, mgr, &[], cid, goal_id)
            .expect("SDE career goal resolves");
        assert_eq!(from_sde.required_progress, 5);
        assert_eq!(from_sde.sp_reward, 1_250.0);
        assert_eq!(from_sde.career_points_progress, None);

        // Passing a milestone with the same ID prioritizes milestone metadata and populates career_points_progress.
        let override_milestone = CareerMilestoneDef {
            milestone_id: goal_id,
            career_id: Some(1),
            required_points: 0, // verifies `.max(1)` floor on required_progress
            sp_reward: 12_345.0,
            description: "Test Milestone Override".to_string(),
        };
        let from_milestone = resolve_career_goal_or_milestone(
            &sde,
            mgr,
            &[override_milestone],
            cid,
            goal_id,
        )
        .expect("Milestone resolves ahead of SDE");
        assert_eq!(from_milestone.required_progress, 1);
        assert_eq!(from_milestone.sp_reward, 12_345.0);
        assert!(from_milestone.career_points_progress.is_some());
    }

    #[test]
    fn claim_career_goal_or_milestone_validates_progress_missing_char_duplicate_and_credits_sp() {
        let ctx = shared_test_ctx();
        let mgr = &ctx.quasar_gateway.progression.career_goals;
        let cid = 91_000_004i64;
        let goal_id = 55_001u32;

        let mut snap = WorldStateSnapshot::default();
        // Progress incomplete fails before character lookup.
        let err_incomplete =
            claim_career_goal_or_milestone_in_snap(&mut snap, mgr, cid, goal_id, 4, 5, 1_000.0)
                .unwrap_err();
        assert_eq!(err_incomplete.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_incomplete.code, "GoalProgressIncomplete");

        // Missing character fails after progress check.
        let err_missing =
            claim_career_goal_or_milestone_in_snap(&mut snap, mgr, cid, goal_id, 5, 5, 1_000.0)
                .unwrap_err();
        assert_eq!(err_missing.kind, DomainErrorKind::NotFound);
        assert_eq!(err_missing.code, "CharacterNotFound");

        // Seed character and complete claim.
        seed_char(&mut snap, make_char(cid, 3, 98_000_020, None, 0));
        let total_free_sp =
            claim_career_goal_or_milestone_in_snap(&mut snap, mgr, cid, goal_id, 5, 5, 3_500.0)
                .unwrap();
        assert_eq!(total_free_sp, 3_500.0);
        assert_eq!(snap.characters[&cid].free_skill_points, 3_500.0);
        assert!(snap.npe_progress[&cid].claimed_goals.contains(&goal_id));
        assert!(mgr.is_milestone_claimed(cid as u32, goal_id, &snap));

        // Duplicate claim rejected when already present in snapshot / manager.
        let err_dup =
            claim_career_goal_or_milestone_in_snap(&mut snap, mgr, cid, goal_id, 5, 5, 3_500.0)
                .unwrap_err();
        assert_eq!(err_dup.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_dup.code, "GoalAlreadyClaimed");

        // Even if npe_progress entry already had another goal claimed independently, npe_progress gate blocks duplicate.
        let goal_id_2 = 55_002u32;
        snap.npe_progress
            .entry(cid)
            .or_insert_with(|| NpeProgressRecord {
                character_id: cid,
                node_graph_id: 0,
                current_node_id: 0,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            })
            .claimed_goals
            .insert(goal_id_2);
        let err_dup2 =
            claim_career_goal_or_milestone_in_snap(&mut snap, mgr, cid, goal_id_2, 1, 1, 500.0)
                .unwrap_err();
        assert_eq!(err_dup2.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_dup2.code, "GoalAlreadyClaimed");
    }
}

// ---------------------------------------------------------------------------
// D2: corp_governance
// ---------------------------------------------------------------------------
mod corp_governance_tests {
    use super::*;
    use crate::domain::corp_governance::{
        ALLOWED_CORP_MAIL_ROLES, CORP_ROLE_DEFINITIONS, EffectiveCorpRoles, can_send_corp_mail,
        corp_member_limit_from_skills, decode_corp_roles, effective_corp_member_roles,
        effective_corp_role_breakdown, encode_esi_corp_roles, resolve_corp_member_limit,
    };
    use eve_db::corp_roles;
    use eve_db::durable_domain_types::CorpGovState;

    #[test]
    fn corp_role_decode_and_encode_round_trip_and_skip_unmapped_bit() {
        assert!(decode_corp_roles(0).is_empty());
        assert_eq!(encode_esi_corp_roles(&[]), 0);
        assert_eq!(encode_esi_corp_roles(&["NoSuchRole".to_string()]), 0);

        let mapped_mask: u64 = CORP_ROLE_DEFINITIONS
            .iter()
            .filter(|(_, _, esi)| !esi.is_empty())
            .fold(0u64, |acc, &(bit, _, _)| acc | bit);
        let decoded = decode_corp_roles(mapped_mask);
        assert!(!decoded.is_empty());
        assert!(decoded.contains(&"Director"));
        assert!(decoded.contains(&"Hangar_Take_1"));
        assert!(decoded.contains(&"Container_Take_7"));

        let decoded_strings: Vec<String> = decoded.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(encode_esi_corp_roles(&decoded_strings), mapped_mask);

        // Unmapped bits (where esi_name == "") are omitted from decode_corp_roles output.
        for &(unmapped_bit, client_name, esi_name) in CORP_ROLE_DEFINITIONS {
            if esi_name.is_empty() {
                assert!(decode_corp_roles(unmapped_bit).is_empty());
                assert_eq!(
                    encode_esi_corp_roles(&[client_name.to_ascii_lowercase()]),
                    unmapped_bit
                );
            }
        }

        // Case-insensitive acceptance for both ESI snake/title names and client display names.
        let mixed = vec![
            "director".to_string(),
            "hAnGaR cAn TaKe 1".to_string(),
            "station_manager".to_string(),
        ];
        assert_eq!(
            encode_esi_corp_roles(&mixed),
            corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_HANGAR_CAN_TAKE[0]
                | corp_roles::ROLE_STATION_MANAGER
        );
    }

    #[test]
    fn effective_corp_role_breakdown_combines_assignments_pending_grants_and_titles_with_stasis() {
        let now = 1_700_000_500u64;
        let corp_id = 98_100_001i64;
        let ceo_id = 92_000_001i64;
        let member_id = 92_000_002i64;
        let outsider_id = 92_000_999i64;

        let mut snap = WorldStateSnapshot::default();
        // Missing corporation fallback.
        assert_eq!(
            effective_corp_member_roles(&snap, corp_id, member_id, 0xABCD, now),
            (false, 0xABCD)
        );
        assert_eq!(
            effective_corp_role_breakdown(&snap, corp_id, member_id, now),
            EffectiveCorpRoles::default()
        );

        let mut corp = make_corp(corp_id, ceo_id, None, &[ceo_id, member_id]);
        // Give CEO no explicit assignment to prove CEO still gets full director/all breakdown.
        corp.member_roles.insert(member_id, corp_roles::ROLE_AUDITOR);
        snap.corporations.insert(corp_id, corp);

        // CEO flag is populated, while stored roles remain explicit unless assigned.
        let ceo_breakdown = effective_corp_role_breakdown(&snap, corp_id, ceo_id, now);
        assert!(ceo_breakdown.is_ceo);
        assert_eq!(ceo_breakdown.roles, 0);
        let (is_ceo_flag, ceo_session_roles) =
            effective_corp_member_roles(&snap, corp_id, ceo_id, 0xBEEF, now);
        assert!(is_ceo_flag);
        assert_eq!(ceo_session_roles, 0);

        // Outsider has no roles.
        assert_eq!(
            effective_corp_role_breakdown(&snap, corp_id, outsider_id, now),
            EffectiveCorpRoles::default()
        );

        // Member role assignments across HQ/Base/Other + grantables.
        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                roles: corp_roles::ROLE_SECURITY_OFFICER,
                grantable_roles: corp_roles::ROLE_ACCOUNTANT,
                roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                grantable_roles_at_hq: corp_roles::ROLE_HANGAR_CAN_QUERY[0],
                roles_at_base: corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                grantable_roles_at_base: corp_roles::ROLE_HANGAR_CAN_QUERY[1],
                roles_at_other: corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                grantable_roles_at_other: corp_roles::ROLE_HANGAR_CAN_QUERY[2],
            },
        );

        // Add snapshot corp titles (title_id 1 via corp_member_titles, 64 via gov.member_titles)
        // plus governance titles (2 active via bit 1, 0 and 65 out-of-range ignored).
        snap.corp_titles.insert(
            corp_id,
            vec![
                CorpTitleEntry {
                    title_id: 1,
                    name: "Quartermaster".to_string(),
                    roles: vec!["Factory_Manager".to_string()],
                    grantable_roles: vec!["Station_Manager".to_string()],
                    roles_at_hq: vec!["Hangar_Take_4".to_string()],
                    ..Default::default()
                },
                CorpTitleEntry {
                    title_id: 64,
                    name: "Corp Title Ignored By Gov Bitmask".to_string(),
                    roles: vec![],
                    ..Default::default()
                },
            ],
        );
        snap.corp_member_titles.insert((corp_id, member_id), vec![1]);

        // Governance pending role grant (stasis active until now + 100), pending title grant,
        // and governance titles (title_id 2 & 64 valid 1..=64; 0 and 65 ignored).
        let mut gov = CorpGovState::default();
        gov.titles.insert(
            2,
            ("Gov Personnel".to_string(), corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_BRAND_MANAGER),
        );
        gov.titles
            .insert(64, ("Edge Title 64".to_string(), corp_roles::ROLE_DIPLOMAT));
        gov.titles
            .insert(0, ("Out Of Range Zero".to_string(), corp_roles::ROLE_DIRECTOR));
        gov.titles
            .insert(65, ("Out Of Range 65".to_string(), corp_roles::ROLE_DIRECTOR));
        gov.member_titles
            .insert(member_id, (1u64 << 1) | (1u64 << 63));
        gov.pending_role_grants
            .insert(member_id, (corp_roles::ROLE_DIRECTOR, now + 100));
        gov.pending_title_role_grants
            .insert(member_id, (corp_roles::ROLE_BRAND_MANAGER, now + 100));
        snap.corp_governance.insert(corp_id, gov);

        // Before maturity: stasis hides pending role (ROLE_DIRECTOR) and pending title bit (ROLE_BRAND_MANAGER),
        // while existing corp_member_titles + non-pending gov.titles (title 2 Personnel, title 64 Diplomat) apply,
        // and out-of-range gov.titles (0, 65) never leak ROLE_DIRECTOR.
        let before = effective_corp_role_breakdown(&snap, corp_id, member_id, now);
        assert!(!before.is_ceo);
        assert_eq!(before.roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_eq!(before.roles & corp_roles::ROLE_BRAND_MANAGER, 0);
        assert_ne!(before.roles & corp_roles::ROLE_AUDITOR, 0);
        assert_ne!(before.roles & corp_roles::ROLE_SECURITY_OFFICER, 0);
        assert_ne!(before.roles & corp_roles::ROLE_PERSONNEL_MANAGER, 0);
        assert_ne!(before.roles & corp_roles::ROLE_FACTORY_MANAGER, 0);
        assert_ne!(before.roles & corp_roles::ROLE_DIPLOMAT, 0);

        let any_active = before.any_active_roles();
        assert_ne!(any_active & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
        assert_ne!(any_active & corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
        assert_ne!(any_active & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);
        assert_ne!(any_active & corp_roles::ROLE_HANGAR_CAN_TAKE[3], 0);

        let any_grant = before.any_grantable_roles();
        assert_ne!(any_grant & corp_roles::ROLE_ACCOUNTANT, 0);
        assert_ne!(any_grant & corp_roles::ROLE_STATION_MANAGER, 0);
        assert_ne!(any_grant & corp_roles::ROLE_HANGAR_CAN_QUERY[0], 0);
        assert_ne!(any_grant & corp_roles::ROLE_HANGAR_CAN_QUERY[1], 0);
        assert_ne!(any_grant & corp_roles::ROLE_HANGAR_CAN_QUERY[2], 0);

        // Distinguish `effective_corp_member_roles` (only delays DELAYED_STASIS_MASK) vs full breakdown.
        // Place a non-stasis immediate role (ROLE_ACCOUNTANT) into corp_role_assignments.roles while keeping it
        // in pending_role_grants: full breakdown hides it before maturity, effective_corp_member_roles preserves it.
        snap.corp_role_assignments
            .get_mut(&(corp_id, member_id))
            .unwrap()
            .roles |= corp_roles::ROLE_ACCOUNTANT;
        snap.corp_governance
            .get_mut(&corp_id)
            .unwrap()
            .pending_role_grants
            .insert(
                member_id,
                (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT, now + 100),
            );
        let before_with_acct = effective_corp_role_breakdown(&snap, corp_id, member_id, now);
        assert_eq!(before_with_acct.roles & corp_roles::ROLE_ACCOUNTANT, 0);
        let (_, mem_roles_before) =
            effective_corp_member_roles(&snap, corp_id, member_id, 0, now);
        assert_ne!(mem_roles_before & corp_roles::ROLE_ACCOUNTANT, 0);
        assert_eq!(mem_roles_before & corp_roles::ROLE_DIRECTOR, 0);

        // After maturity: pending general and title grants all apply.
        let after = effective_corp_role_breakdown(&snap, corp_id, member_id, now + 100);
        assert_ne!(after.roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_ne!(after.roles & corp_roles::ROLE_BRAND_MANAGER, 0);

        let (m_ceo, m_roles_after) =
            effective_corp_member_roles(&snap, corp_id, member_id, 0, now + 100);
        assert!(!m_ceo);
        assert_ne!(m_roles_after & corp_roles::ROLE_DIRECTOR, 0);
    }

    #[test]
    fn can_send_corp_mail_respects_restrict_flag_ceo_and_allowed_roles() {
        let now = 1_700_000_000u64;
        let corp_id = 98_100_002i64;
        let ceo_id = 92_000_101i64;
        let officer_id = 92_000_102i64;
        let grunt_id = 92_000_103i64;
        let outsider_id = 92_000_199i64;

        let mut snap = WorldStateSnapshot::default();
        snap.corporations
            .insert(corp_id, make_corp(corp_id, ceo_id, None, &[ceo_id, officer_id, grunt_id]));

        // Without restrict_corp_mail (missing or false), everyone passes.
        assert!(can_send_corp_mail(&snap, corp_id, grunt_id, now));
        assert!(can_send_corp_mail(&snap, corp_id, outsider_id, now));

        // Enable restrict_corp_mails.
        let mut gov = CorpGovState::default();
        gov.restrict_corp_mails = true;
        snap.corp_governance.insert(corp_id, gov);

        assert!(can_send_corp_mail(&snap, corp_id, ceo_id, now));
        assert!(!can_send_corp_mail(&snap, corp_id, grunt_id, now));
        assert!(!can_send_corp_mail(&snap, corp_id, outsider_id, now));

        // Non-mail role (outside ALLOWED_CORP_MAIL_ROLES) stays denied.
        let disallowed_role = corp_roles::ROLE_HANGAR_CAN_TAKE[0];
        assert_eq!(disallowed_role & ALLOWED_CORP_MAIL_ROLES, 0);
        snap.corp_role_assignments.insert(
            (corp_id, grunt_id),
            CorpMemberRoleAssignment {
                character_id: grunt_id,
                roles: disallowed_role,
                ..Default::default()
            },
        );
        assert!(!can_send_corp_mail(&snap, corp_id, grunt_id, now));

        // Officer holding an allowed role bit (e.g. Personnel Manager / Communications / Director) succeeds.
        let allowed_bit = ALLOWED_CORP_MAIL_ROLES & (!corp_roles::ROLE_DIRECTOR);
        let single_allowed = 1u64 << allowed_bit.trailing_zeros();
        snap.corp_role_assignments.insert(
            (corp_id, officer_id),
            CorpMemberRoleAssignment {
                character_id: officer_id,
                roles: single_allowed,
                ..Default::default()
            },
        );
        assert!(can_send_corp_mail(&snap, corp_id, officer_id, now));
    }

    #[test]
    fn corp_member_limit_from_skills_and_resolve_corp_member_limit_precedence() {
        let ctx = shared_test_ctx();
        let corp_id = 98_100_003i64;
        let ceo_id = 92_000_201i64;
        let mut snap = WorldStateSnapshot::default();

        // Missing corp returns None.
        assert_eq!(resolve_corp_member_limit(Some(&ctx.sde), &snap, corp_id), None);

        snap.corporations
            .insert(corp_id, make_corp(corp_id, ceo_id, None, &[ceo_id]));

        // Base 10 when CEO has 0 trained skills.
        assert_eq!(corp_member_limit_from_skills(&ctx.sde, &snap, ceo_id), 10);
        assert_eq!(resolve_corp_member_limit(Some(&ctx.sde), &snap, corp_id), Some(10));
        assert_eq!(resolve_corp_member_limit(None, &snap, corp_id), Some(10));

        // Find real corp-member-limit skills (dogma attribute 191) from SDE.
        let mut attr191_skills: Vec<(u32, i64)> = ctx
            .sde
            .types
            .iter()
            .filter_map(|(&tid, td)| {
                td.default_attributes
                    .get(&191)
                    .copied()
                    .filter(|&v| v.round() > 0.0)
                    .map(|v| (tid, v.round() as i64))
            })
            .collect();
        attr191_skills.sort_by_key(|&(tid, _)| tid);
        assert!(
            !attr191_skills.is_empty(),
            "SDE must define attribute 191 skills for corp member limit"
        );

        let (skill_tid, bonus_per_level) = attr191_skills[0];
        let mut ceo_skills = HashMap::new();
        ceo_skills.insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 3,
                skill_points: 10_000.0,
            },
        );
        snap.skills.insert(ceo_id, ceo_skills);
        let expected_skill_limit = 3 * bonus_per_level;
        assert_eq!(
            corp_member_limit_from_skills(&ctx.sde, &snap, ceo_id),
            expected_skill_limit
        );
        assert_eq!(
            resolve_corp_member_limit(Some(&ctx.sde), &snap, corp_id),
            Some(expected_skill_limit)
        );
        // Without SDE, falls back to 10.
        assert_eq!(resolve_corp_member_limit(None, &snap, corp_id), Some(10));

        // Governance member_limit overrides skill calculation.
        let mut gov = CorpGovState::default();
        gov.member_limit = Some(42);
        snap.corp_governance.insert(corp_id, gov);
        assert_eq!(resolve_corp_member_limit(Some(&ctx.sde), &snap, corp_id), Some(42));
        assert_eq!(resolve_corp_member_limit(None, &snap, corp_id), Some(42));

        // Snapshot corp_member_limits takes highest precedence.
        snap.corp_member_limits.insert(corp_id, 120);
        assert_eq!(resolve_corp_member_limit(Some(&ctx.sde), &snap, corp_id), Some(120));
    }

    #[test]
    fn corp_governance_bitmasks_title_scopes_maturity_boundaries_and_stasis_details() {
        let ctx = shared_test_ctx();
        let corp_id = 98_110_001i64;
        let ceo_id = 92_100_001i64;
        let member_id = 92_100_002i64;
        let matured_at = 1_700_010_000u64;

        // Empty string never matches unmapped entries with esi_name == ""; unmapped bit skipped in decode_corp_roles.
        assert_eq!(encode_esi_corp_roles(&[String::new()]), 0);
        assert_eq!(
            decode_corp_roles(
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER
            ),
            vec!["Director"]
        );

        let mut snap = WorldStateSnapshot::default();
        snap.corporations
            .insert(corp_id, make_corp(corp_id, ceo_id, None, &[ceo_id, member_id]));

        // Seed CorpTitleEntry with ALL 8 role fields populated and a gov title at tid=1 (`1u64 << 0`).
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "AllScopes".into(),
                roles: vec!["Trader".into()],
                roles_at_hq: vec!["Communications_Officer".into()],
                roles_at_base: vec!["Hangar_Take_2".into()],
                roles_at_other: vec!["Hangar_Take_3".into()],
                grantable_roles: vec!["Auditor".into()],
                grantable_roles_at_hq: vec!["Hangar_Query_1".into()],
                grantable_roles_at_base: vec!["Hangar_Query_2".into()],
                grantable_roles_at_other: vec!["Hangar_Query_3".into()],
            }],
        );
        snap.corp_member_titles.insert((corp_id, member_id), vec![1]);

        let mut gov = CorpGovState::default();
        gov.restrict_corp_mails = true;
        gov.titles
            .insert(1, ("Title1".into(), corp_roles::ROLE_CONTRACT_MANAGER));
        gov.member_titles.insert(member_id, 1u64);

        // Include delayed hangar/account/container stasis bits + immediate Accountant/CommunicationsOfficer.
        let delayed_mask = corp_roles::ROLE_DIRECTOR
            | corp_roles::ROLE_HANGAR_CAN_TAKE[0]
            | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
            | corp_roles::ROLE_CONTAINER_CAN_TAKE[6];
        let pending_roles = delayed_mask | corp_roles::ROLE_ACCOUNTANT;
        let pending_title_mask = corp_roles::ROLE_CHAT_MANAGER | corp_roles::ROLE_TRADER;
        gov.pending_role_grants
            .insert(member_id, (pending_roles, matured_at));
        gov.pending_title_role_grants
            .insert(member_id, (pending_title_mask, matured_at));
        snap.corp_governance.insert(corp_id, gov);

        // Assign pending roles to member_roles too so we test both full masking and DELAYED_STASIS_MASK masking.
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, delayed_mask | corp_roles::ROLE_ACCOUNTANT);

        // At matured_at - 1: unmatured_mask suppresses CommunicationsOfficer (roles_at_hq), Trader, Director, etc.
        let bd_before = effective_corp_role_breakdown(&snap, corp_id, member_id, matured_at - 1);
        assert_eq!(bd_before.unmatured_mask, pending_roles | pending_title_mask);
        assert_eq!(bd_before.roles & corp_roles::ROLE_TRADER, 0);
        assert_eq!(bd_before.roles & corp_roles::ROLE_ACCOUNTANT, 0);
        assert_eq!(bd_before.roles_at_hq & corp_roles::ROLE_CHAT_MANAGER, 0);
        assert_ne!(bd_before.roles & corp_roles::ROLE_CONTRACT_MANAGER, 0);
        assert_ne!(bd_before.roles_at_base & corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
        assert_ne!(bd_before.roles_at_other & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);
        assert_ne!(bd_before.grantable_roles & corp_roles::ROLE_AUDITOR, 0);
        assert_ne!(
            bd_before.grantable_roles_at_hq & corp_roles::ROLE_HANGAR_CAN_QUERY[0],
            0
        );
        assert_ne!(
            bd_before.grantable_roles_at_base & corp_roles::ROLE_HANGAR_CAN_QUERY[1],
            0
        );
        assert_ne!(
            bd_before.grantable_roles_at_other & corp_roles::ROLE_HANGAR_CAN_QUERY[2],
            0
        );
        // Corp mail denied while ROLE_CHAT_MANAGER (hq) & ROLE_DIRECTOR are masked.
        assert!(!can_send_corp_mail(&snap, corp_id, member_id, matured_at - 1));

        // effective_corp_member_roles at matured_at - 1 hides Director + Hangar/Account/Container stasis bits, keeps Accountant & ContractManager!
        let (_, mem_before) =
            effective_corp_member_roles(&snap, corp_id, member_id, 0, matured_at - 1);
        assert_eq!(mem_before & corp_roles::ROLE_DIRECTOR, 0);
        assert_eq!(mem_before & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
        assert_eq!(mem_before & corp_roles::ROLE_ACCOUNT_CAN_TAKE[0], 0);
        assert_eq!(mem_before & corp_roles::ROLE_CONTAINER_CAN_TAKE[6], 0);
        assert_ne!(mem_before & corp_roles::ROLE_ACCOUNTANT, 0);
        assert_ne!(mem_before & corp_roles::ROLE_CONTRACT_MANAGER, 0);

        // At exact matured_at (>= boundary): all pending role & title grants mature (`unmatured_mask == 0`).
        let bd_at = effective_corp_role_breakdown(&snap, corp_id, member_id, matured_at);
        assert_eq!(bd_at.unmatured_mask, 0);
        assert_ne!(bd_at.roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_ne!(bd_at.roles & corp_roles::ROLE_TRADER, 0);
        assert_ne!(bd_at.roles_at_hq & corp_roles::ROLE_CHAT_MANAGER, 0);
        let (_, mem_at) = effective_corp_member_roles(&snap, corp_id, member_id, 0, matured_at);
        assert_ne!(mem_at & corp_roles::ROLE_CONTAINER_CAN_TAKE[6], 0);

        // can_send_corp_mail uses `any_active_roles()` -> succeeds when ROLE_CHAT_MANAGER is ONLY in roles_at_hq!
        snap.corp_governance.get_mut(&corp_id).unwrap().pending_role_grants.clear();
        snap.corp_governance.get_mut(&corp_id).unwrap().pending_title_role_grants.clear();
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
        snap.corp_governance.get_mut(&corp_id).unwrap().member_titles.clear();
        snap.corp_titles.get_mut(&corp_id).unwrap()[0].roles.clear();
        assert!(can_send_corp_mail(&snap, corp_id, member_id, matured_at));

        // Non-attr-191 skills compute 0 bonus -> limit falls back to 10; multiple attr-191 skills sum!
        let mut no_191_skills = HashMap::new();
        if let Some((&other_tid, _)) = ctx
            .sde
            .types
            .iter()
            .find(|(_, td)| td.category_id == 16 && !td.default_attributes.contains_key(&191))
        {
            no_191_skills.insert(
                other_tid,
                TrainedSkillRecord {
                    type_id: other_tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.skills.insert(ceo_id, no_191_skills);
        assert_eq!(corp_member_limit_from_skills(&ctx.sde, &snap, ceo_id), 10);

        let mut attr191_skills: Vec<(u32, i64)> = ctx
            .sde
            .types
            .iter()
            .filter_map(|(&tid, td)| {
                td.default_attributes
                    .get(&191)
                    .copied()
                    .filter(|&v| v.round() > 0.0)
                    .map(|v| (tid, v.round() as i64))
            })
            .collect();
        attr191_skills.sort_by_key(|&(tid, _)| tid);
        let mut sum_expected = 0i64;
        let multi_skills = snap.skills.get_mut(&ceo_id).unwrap();
        for (idx, (tid, per_lvl)) in attr191_skills.iter().take(2).enumerate() {
            let lvl = (idx as u8) + 2;
            sum_expected += (*per_lvl) * i64::from(lvl);
            multi_skills.insert(
                *tid,
                TrainedSkillRecord {
                    type_id: *tid,
                    level: lvl,
                    skill_points: 10_000.0,
                },
            );
        }
        assert_eq!(
            corp_member_limit_from_skills(&ctx.sde, &snap, ceo_id),
            sum_expected
        );
    }
}

// ---------------------------------------------------------------------------
// D3: mail & CSPA
// ---------------------------------------------------------------------------
mod mail_tests {
    use super::*;
    use crate::domain::mail::{
        CspaCalculationInput, MailRecipientKind, MailRecipientSpec, SendMailInput,
        calculate_cspa_fee, charge_cspa_action, send_mail,
    };
    use crate::esi::runtime_state::EsiContactEntry;
    use crate::services::fleet_social::fleet_social_persistence::MailingListState;
    use eve_db::corp_roles;
    use eve_db::durable_domain_types::CorpGovState;

    #[test]
    fn calculate_cspa_fee_checks_missing_char_self_zero_corp_alliance_and_three_contact_stores() {
        let ctx = shared_test_ctx();
        let caller_id = 93_000_001i64;
        let same_corp_target = 93_000_002i64;
        let same_ally_target = 93_000_003i64;
        let snap_contact_exempt = 93_000_004i64;
        let snap_contact_eq_boundary = 93_000_005i64;
        let esi_contact_exempt = 93_000_006i64;
        let social_contact_exempt = 93_000_007i64;
        let blocked_friendly_target = 93_000_008i64;
        let zero_charge_target = 93_000_009i64;
        let custom_charge_target = 93_000_010i64;

        let caller_corp = 98_200_001i64;
        let ally_corp = 98_200_002i64;
        let other_corp = 98_200_003i64;
        let alliance_id = 99_200_001i64;

        // Missing caller -> CharacterNotFound.
        let err_missing = calculate_cspa_fee(
            &ctx,
            &CspaCalculationInput {
                caller_id: 999_777_666,
                target_ids: &[same_corp_target],
                default_charge: IskCents::from_isk(2_950),
                exempt_on_same_alliance: true,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap_err();
        assert_eq!(err_missing.kind, DomainErrorKind::NotFound);
        assert_eq!(err_missing.code, "CharacterNotFound");

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(caller_id, 1, caller_corp, Some(alliance_id), 1_000_000));
            seed_char(
                snap,
                make_char(same_corp_target, 2, caller_corp, Some(alliance_id), 1_000),
            );
            seed_char(
                snap,
                make_char(same_ally_target, 3, ally_corp, Some(alliance_id), 1_000),
            );
            for cid in [
                snap_contact_exempt,
                snap_contact_eq_boundary,
                esi_contact_exempt,
                social_contact_exempt,
                blocked_friendly_target,
                zero_charge_target,
                custom_charge_target,
            ] {
                seed_char(snap, make_char(cid, 4, other_corp, None, 1_000));
            }

            // Store 1: snap.char_contacts (standing > 0.0 exempt; standing == 0.0 NOT exempt).
            snap.char_contacts.insert(
                snap_contact_exempt,
                vec![SocialContactRecord {
                    contact_id: caller_id,
                    contact_type: "character".to_string(),
                    standing: 0.1,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: Vec::new(),
                }],
            );
            snap.char_contacts.insert(
                snap_contact_eq_boundary,
                vec![SocialContactRecord {
                    contact_id: caller_id,
                    contact_type: "character".to_string(),
                    standing: 0.0,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: Vec::new(),
                }],
            );
            // Blocked overrides positive standing.
            snap.char_contacts.insert(
                blocked_friendly_target,
                vec![SocialContactRecord {
                    contact_id: caller_id,
                    contact_type: "character".to_string(),
                    standing: 10.0,
                    is_watched: false,
                    is_blocked: true,
                    label_ids: Vec::new(),
                }],
            );

            // Charge precedence: snap.character_cspa_charges beats social_extras beats default_charge.
            snap.character_cspa_charges
                .insert(zero_charge_target, IskCents(0));
            snap.character_cspa_charges
                .insert(custom_charge_target, IskCents::from_isk(5_000));
        });

        // Store 2: esi_state.char_contacts.
        ctx.esi_state.char_contacts.write().unwrap().insert(
            esi_contact_exempt,
            vec![EsiContactEntry {
                contact_id: caller_id,
                contact_type: "character".to_string(),
                standing: 5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );

        // Store 3: social_extras.contacts & social_extras.cspa_charges (overridden by snap.character_cspa_charges).
        {
            let mut se = ctx.social_extras.lock().unwrap();
            se.contacts
                .entry(social_contact_exempt)
                .or_default()
                .insert(caller_id, (2.5, false, 0));
            se.contacts
                .entry(blocked_friendly_target)
                .or_default()
                .insert(caller_id, (10.0, false, 0));
            se.blocked
                .entry(blocked_friendly_target)
                .or_default()
                .insert(caller_id);
            se.cspa_charges
                .insert(custom_charge_target, IskCents::from_isk(9_999));
            se.cspa_charges
                .insert(snap_contact_eq_boundary, IskCents::from_isk(1_200));
        }

        // With exempt_on_same_alliance = true:
        // Skipped/0: caller_id, -5, 0, same_corp, same_ally, snap_contact_exempt,
        //            esi_contact_exempt, social_contact_exempt, zero_charge_target
        // Charged: snap_contact_eq_boundary (1_200), blocked_friendly_target (default 2_950),
        //          custom_charge_target (snap 5_000 beats social 9_999)
        let targets = [
            caller_id,
            -5,
            0,
            same_corp_target,
            same_ally_target,
            snap_contact_exempt,
            snap_contact_eq_boundary,
            esi_contact_exempt,
            social_contact_exempt,
            blocked_friendly_target,
            zero_charge_target,
            custom_charge_target,
        ];
        let fee_ally_exempt = calculate_cspa_fee(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &targets,
                default_charge: IskCents::from_isk(2_950),
                exempt_on_same_alliance: true,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap();
        assert_eq!(fee_ally_exempt, IskCents::from_isk(1_200 + 2_950 + 5_000));

        // With exempt_on_same_alliance = false, same_ally_target also pays default_charge (2_950).
        let fee_ally_charged = calculate_cspa_fee(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &[same_ally_target],
                default_charge: IskCents::from_isk(2_950),
                exempt_on_same_alliance: false,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap();
        assert_eq!(fee_ally_charged, IskCents::from_isk(2_950));
    }

    #[test]
    fn charge_cspa_action_checks_zero_fee_insufficient_funds_and_debit() {
        let ctx = shared_test_ctx();
        let caller_id = 93_000_020i64;
        let same_corp = 93_000_021i64;
        let other_target = 93_000_022i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(caller_id, 1, 98_200_010, None, 1_000));
            seed_char(snap, make_char(same_corp, 2, 98_200_010, None, 0));
            seed_char(snap, make_char(other_target, 3, 98_200_011, None, 0));
        });

        // Fee 0 does not touch balance.
        let free = charge_cspa_action(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &[same_corp],
                default_charge: IskCents::from_isk(2_950),
                exempt_on_same_alliance: true,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap();
        assert_eq!(free, IskCents(0));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&caller_id].balance),
            IskCents::from_isk(1_000)
        );

        // Fee > balance returns InsufficientFundsForCSPA without debit.
        let err_poor = charge_cspa_action(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &[other_target],
                default_charge: IskCents::from_isk(2_500),
                exempt_on_same_alliance: true,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap_err();
        assert_eq!(err_poor.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_poor.code, "InsufficientFundsForCSPA");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&caller_id].balance),
            IskCents::from_isk(1_000)
        );

        // Affordable fee debits caller.
        let charged = charge_cspa_action(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &[other_target],
                default_charge: IskCents::from_isk(600),
                exempt_on_same_alliance: true,
                exempt_min_standing: 0.0,
            },
        )
        .unwrap();
        assert_eq!(charged, IskCents::from_isk(600));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&caller_id].balance),
            IskCents::from_isk(400)
        );
    }

    #[test]
    fn send_mail_validates_subject_body_and_empty_recipients() {
        let ctx = shared_test_ctx();
        let sender_id = 93_000_030i64;
        let recip_id = 93_000_031i64;
        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(sender_id, 1, 98_200_020, None, 50_000));
            seed_char(snap, make_char(recip_id, 2, 98_200_020, None, 0));
        });

        let base = |subject: &str, body: &str, recips: Vec<MailRecipientSpec>| SendMailInput {
            sender_id,
            recipients: recips,
            to_list_id: None,
            to_corp_or_alliance_id: None,
            subject: subject.to_string(),
            body: body.to_string(),
            approved_cost_isk: None,
            max_subject_chars: 10,
            max_body_chars: 20,
            expand_org_recipients_in_db: true,
            record_cspa_journal: false,
            cspa_exempt_min_standing: 0.0,
            cspa_exempt_same_corp: true,
            now_unix: 1_700_000_000,
        };
        let one_recip = || {
            vec![MailRecipientSpec {
                recipient_id: recip_id,
                kind: MailRecipientKind::Character,
            }]
        };

        let err_empty_sub = send_mail(&ctx, base("", "hello", one_recip())).unwrap_err();
        assert_eq!(err_empty_sub.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_empty_sub.code, "InvalidMailSubject");

        let err_long_sub =
            send_mail(&ctx, base("12345678901", "hello", one_recip())).unwrap_err();
        assert_eq!(err_long_sub.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_long_sub.code, "InvalidMailSubject");

        let err_long_body = send_mail(
            &ctx,
            base("ok", &"x".repeat(21), one_recip()),
        )
        .unwrap_err();
        assert_eq!(err_long_body.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_long_body.code, "MailBodyTooLong");

        let err_no_recip = send_mail(&ctx, base("ok", "hello", Vec::new())).unwrap_err();
        assert_eq!(err_no_recip.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_no_recip.code, "InvalidRecipient");
    }

    #[test]
    fn send_mail_mailing_list_unknown_access_denied_muted_and_paid_fee() {
        let ctx = shared_test_ctx();
        let owner_id = 93_000_040i64;
        let subscriber_id = 93_000_041i64;
        let muted_id = 93_000_042i64;
        let outsider_id = 93_000_043i64;
        let list_id = 555_001i64;
        let corp_id = 98_200_030i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(owner_id, 1, corp_id, None, 1_000));
            seed_char(snap, make_char(subscriber_id, 2, corp_id, None, 10_000));
            seed_char(snap, make_char(muted_id, 3, corp_id, None, 10_000));
            seed_char(snap, make_char(outsider_id, 4, corp_id, None, 10_000));
        });

        let make_list_send = |sender_id: i64, lid: i64| SendMailInput {
            sender_id,
            recipients: vec![MailRecipientSpec {
                recipient_id: lid,
                kind: MailRecipientKind::MailingList,
            }],
            to_list_id: Some(lid),
            to_corp_or_alliance_id: None,
            subject: "List News".to_string(),
            body: "Hello list".to_string(),
            approved_cost_isk: Some(10_000),
            max_subject_chars: 100,
            max_body_chars: 1_000,
            expand_org_recipients_in_db: true,
            record_cspa_journal: false,
            cspa_exempt_min_standing: 0.0,
            cspa_exempt_same_corp: true,
            now_unix: 1_700_000_000,
        };

        // 1. Unknown mailing list -> NotFound("UserError") with HTTP 400 override.
        let err_missing = send_mail(&ctx, make_list_send(subscriber_id, 999_999)).unwrap_err();
        assert_eq!(err_missing.kind, DomainErrorKind::NotFound);
        assert_eq!(err_missing.code, "UserError");
        assert_eq!(err_missing.http_status(), 400);

        // Seed paid mailing list in snapshot & social_extras.
        let mut members = HashMap::new();
        members.insert(owner_id, 2i64);
        members.insert(subscriber_id, 1i64);
        members.insert(muted_id, 0i64); // muted!
        let mut entity_access = HashMap::new();
        entity_access.insert(subscriber_id, 1i64);
        entity_access.insert(muted_id, 1i64);
        entity_access.insert(outsider_id, 0i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.mailing_lists.insert(
                list_id,
                MailingListRecord {
                    list_id,
                    name: "VIP List".to_string(),
                    owner_id,
                    default_access: 0,
                    default_role: 1,
                    cost: 2_500, // ISK
                    members: members.clone(),
                    entity_access: entity_access.clone(),
                    welcome_mail: None,
                    mail_ids: HashSet::new(),
                },
            );
        });
        ctx.social_extras.lock().unwrap().mailing_lists.insert(
            list_id,
            MailingListState {
                list_id,
                name: "VIP List".to_string(),
                owner_id,
                default_access: 0,
                default_role: 1,
                cost: 2_500,
                members,
                entity_access,
                welcome_mail: None,
                mail_ids: HashSet::new(),
            },
        );

        // 2. Non-owner unsubscribed & entity_access <= 0 -> Forbidden / MailingListAccessDenied.
        let err_denied = send_mail(&ctx, make_list_send(outsider_id, list_id)).unwrap_err();
        assert_eq!(err_denied.kind, DomainErrorKind::Forbidden);
        assert!(err_denied.message.contains("MailingListAccessDenied"));

        // 3. Muted member (role == 0) -> Forbidden / MutedInMailingList.
        let err_muted = send_mail(&ctx, make_list_send(muted_id, list_id)).unwrap_err();
        assert_eq!(err_muted.kind, DomainErrorKind::Forbidden);
        assert!(err_muted.message.contains("MutedInMailingList"));

        // 4. Paid subscriber succeeds: mailing list fee (2_500 ISK) transfers from subscriber to owner_id
        // while `cspa_charged` tracks per-character CSPA charges separately (`0` here).
        let res = send_mail(&ctx, make_list_send(subscriber_id, list_id)).unwrap();
        assert_eq!(res.cspa_charged, IskCents(0));
        assert_eq!(res.to_list_id, Some(list_id));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&subscriber_id].balance, IskCents::from_isk(7_500));
            assert_eq!(snap.characters[&owner_id].balance, IskCents::from_isk(3_500));
            assert!(snap.mailing_lists[&list_id].mail_ids.contains(&res.mail_id));
        });
        assert!(ctx
            .social_extras
            .lock()
            .unwrap()
            .mailing_lists[&list_id]
            .mail_ids
            .contains(&res.mail_id));
    }

    #[test]
    fn send_mail_blocks_contacts_checks_npc_and_restricted_corp_and_alliance_permissions() {
        let ctx = shared_test_ctx();
        let sender_id = 93_000_050i64;
        let ceo_id = 93_000_051i64;
        let blocker_id = 93_000_052i64;
        let ally_member_id = 93_000_053i64;
        let outsider_id = 93_000_054i64;

        let player_corp = 98_200_040i64;
        let ally_corp = 98_200_041i64;
        let other_corp = 98_200_042i64;
        let alliance_id = 99_200_040i64;

        let npc_corp_id = *ctx
            .sde
            .npc_corporations
            .keys()
            .next()
            .expect("SDE must have NPC corps");

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(
                snap,
                make_char(sender_id, 1, player_corp, Some(alliance_id), 100_000),
            );
            seed_char(
                snap,
                make_char(ceo_id, 2, player_corp, Some(alliance_id), 100_000),
            );
            seed_char(snap, make_char(blocker_id, 3, other_corp, None, 1_000));
            seed_char(
                snap,
                make_char(ally_member_id, 4, ally_corp, Some(alliance_id), 1_000),
            );
            seed_char(snap, make_char(outsider_id, 5, other_corp, None, 1_000));

            snap.corporations.insert(
                player_corp,
                make_corp(player_corp, ceo_id, Some(alliance_id), &[ceo_id, sender_id]),
            );
            snap.corporations.insert(
                ally_corp,
                make_corp(ally_corp, ally_member_id, Some(alliance_id), &[ally_member_id]),
            );
            snap.corporations.insert(
                other_corp,
                make_corp(other_corp, outsider_id, None, &[outsider_id, blocker_id]),
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Test Alliance".to_string(),
                    short_name: "TALLY".to_string(),
                    executor_corp_id: player_corp,
                    member_corp_ids: HashSet::from([player_corp, ally_corp]),
                },
            );

            // Blocker explicitly blocks sender (not overridden by positive standing).
            snap.char_contacts.insert(
                blocker_id,
                vec![SocialContactRecord {
                    contact_id: sender_id,
                    contact_type: "character".to_string(),
                    standing: 0.0,
                    is_watched: false,
                    is_blocked: true,
                    label_ids: Vec::new(),
                }],
            );
        });

        let send_to = |sender: i64, recips: Vec<MailRecipientSpec>, expand: bool| SendMailInput {
            sender_id: sender,
            recipients: recips,
            to_list_id: None,
            to_corp_or_alliance_id: None,
            subject: "Subj".to_string(),
            body: "Body".to_string(),
            approved_cost_isk: None,
            max_subject_chars: 100,
            max_body_chars: 1_000,
            expand_org_recipients_in_db: expand,
            record_cspa_journal: false,
            cspa_exempt_min_standing: 0.0,
            cspa_exempt_same_corp: true,
            now_unix: 1_700_000_000,
        };

        // 1. Blocked contact -> Forbidden("ContactBlocked").
        let err_blocked = send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: blocker_id,
                    kind: MailRecipientKind::Character,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_blocked.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_blocked.code, "ContactBlocked");

        // Adding a non-blocked friendly standing overrides is_blocked_by? Wait:
        // explicit is_blocked in snapshot still blocks unless replaced, so test replacing with friendly contact.
        ctx.db.with_mut_snapshot(|snap| {
            snap.char_contacts.insert(
                blocker_id,
                vec![SocialContactRecord {
                    contact_id: sender_id,
                    contact_type: "character".to_string(),
                    standing: 5.0,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: Vec::new(),
                }],
            );
        });
        assert!(send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: blocker_id,
                    kind: MailRecipientKind::Character,
                }],
                true,
            )
        )
        .is_ok());

        // 2. NPC corporation recipient -> CorpMailAccessDenied.
        let err_npc = send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: npc_corp_id,
                    kind: MailRecipientKind::Corporation,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_npc.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_npc.code, "CorpMailAccessDenied");

        // 3. Restrict corp mail -> non-member denied (CorpMailAccessDenied), unauthorized member denied (CorpMailRestricted), CEO allowed.
        ctx.db.with_mut_snapshot(|snap| {
            snap.corp_governance.insert(
                player_corp,
                CorpGovState {
                    restrict_corp_mails: true,
                    ..Default::default()
                },
            );
        });
        let err_non_member = send_mail(
            &ctx,
            send_to(
                outsider_id,
                vec![MailRecipientSpec {
                    recipient_id: player_corp,
                    kind: MailRecipientKind::Corporation,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_non_member.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_non_member.code, "CorpMailAccessDenied");

        let err_restricted = send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: player_corp,
                    kind: MailRecipientKind::Corporation,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_restricted.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_restricted.code, "CorpMailRestricted");

        // Restricted alliance: non-member corp fails, restricted member corp fails, CEO passes.
        let err_ally_outsider = send_mail(
            &ctx,
            send_to(
                outsider_id,
                vec![MailRecipientSpec {
                    recipient_id: alliance_id,
                    kind: MailRecipientKind::Alliance,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_ally_outsider.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_ally_outsider.code, "AllianceMailAccessDenied");

        let err_ally_restricted = send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: alliance_id,
                    kind: MailRecipientKind::Alliance,
                }],
                true,
            ),
        )
        .unwrap_err();
        assert_eq!(err_ally_restricted.kind, DomainErrorKind::Forbidden);
        assert_eq!(err_ally_restricted.code, "CorpMailRestricted");

        // Grant CEO unrestricted delivery and verify expand_org_recipients_in_db=true vs false.
        let expanded = send_mail(
            &ctx,
            send_to(
                ceo_id,
                vec![MailRecipientSpec {
                    recipient_id: player_corp,
                    kind: MailRecipientKind::Corporation,
                }],
                true,
            ),
        )
        .unwrap();
        assert!(expanded.notify_recipient_ids.contains(&sender_id));
        assert!(expanded.notify_recipient_ids.contains(&ceo_id));
        ctx.db.with_snapshot(|snap| {
            let matching: Vec<_> = snap
                .mails
                .iter()
                .filter(|m| m.message_id == expanded.mail_id)
                .collect();
            assert_eq!(matching.len(), 2);
        });

        // Grant communication officer role bit to sender so corp mail without expansion stores corp row cleanly.
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations
                .get_mut(&player_corp)
                .unwrap()
                .member_roles
                .insert(sender_id, corp_roles::ROLE_DIRECTOR);
        });
        let unexpanded = send_mail(
            &ctx,
            send_to(
                sender_id,
                vec![MailRecipientSpec {
                    recipient_id: player_corp,
                    kind: MailRecipientKind::Corporation,
                }],
                false,
            ),
        )
        .unwrap();
        ctx.db.with_snapshot(|snap| {
            let matching: Vec<_> = snap
                .mails
                .iter()
                .filter(|m| m.message_id == unexpanded.mail_id)
                .collect();
            assert_eq!(matching.len(), 1);
            assert_eq!(matching[0].recipient_id, player_corp);
        });
    }

    #[test]
    fn send_mail_enforces_multi_recipient_quota_approved_cost_cspa_journal_labels_and_read_state() {
        let ctx = shared_test_ctx();
        let sender_id = 93_000_060i64;
        let r_full = 93_000_061i64;
        let r_ok = 93_000_062i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(sender_id, 1, 98_200_050, None, 5_000));
            seed_char(snap, make_char(r_full, 2, 98_200_051, None, 0));
            seed_char(snap, make_char(r_ok, 3, 98_200_052, None, 0));
            snap.character_cspa_charges
                .insert(r_full, IskCents::from_isk(2_950));
            snap.character_cspa_charges
                .insert(r_ok, IskCents::from_isk(2_950));

            // Fill r_full with 1,000 non-deleted mails.
            for i in 1..=1_000i64 {
                snap.mails.push(MailRecord {
                    message_id: 100_000 + i,
                    sender_id: r_ok,
                    recipient_id: r_full,
                    subject: "spam".to_string(),
                    body: "spam".to_string(),
                });
            }
        });

        let make_send = |recips: Vec<i64>, approved: Option<i64>, now_unix: u64| SendMailInput {
            sender_id,
            recipients: recips
                .into_iter()
                .map(|recipient_id| MailRecipientSpec {
                    recipient_id,
                    kind: MailRecipientKind::Character,
                })
                .collect(),
            to_list_id: None,
            to_corp_or_alliance_id: None,
            subject: "Quota & CSPA".to_string(),
            body: "Payload".to_string(),
            approved_cost_isk: approved,
            max_subject_chars: 100,
            max_body_chars: 1_000,
            expand_org_recipients_in_db: true,
            record_cspa_journal: true,
            cspa_exempt_min_standing: 0.0,
            cspa_exempt_same_corp: true,
            now_unix,
        };

        // 1. Multi-recipient (> 1 DB recipients) hits MailQuotaExceeded on r_full.
        let err_quota = send_mail(&ctx, make_send(vec![r_full, r_ok], Some(100_000), 1_700_000_000))
            .unwrap_err();
        assert_eq!(err_quota.kind, DomainErrorKind::Conflict);
        assert_eq!(err_quota.code, "MailQuotaExceeded");

        // 2. Single-recipient bypasses multi-recipient quota check, and approved_cost < fee fails.
        let err_approved =
            send_mail(&ctx, make_send(vec![r_full], Some(100), 1_700_000_000)).unwrap_err();
        assert_eq!(err_approved.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_approved.code, "ApprovedCostExceeded");

        // 3. Fee exceeding balance (5,000 ISK < 5,900 ISK across two non-full targets) fails.
        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(93_000_063, 4, 98_200_053, None, 0));
            snap.character_cspa_charges
                .insert(93_000_063, IskCents::from_isk(2_950));
        });
        let err_funds =
            send_mail(&ctx, make_send(vec![r_ok, 93_000_063], None, 1_700_000_000)).unwrap_err();
        assert_eq!(err_funds.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_funds.code, "InsufficientFundsForCSPA");

        // 4. Valid single-recipient send records CSPA charge, wallet journal, Sent/Inbox labels, timestamp, & sender read state.
        let res = send_mail(&ctx, make_send(vec![r_ok], Some(2_950), 1_700_000_123)).unwrap();
        assert_eq!(res.cspa_charged, IskCents::from_isk(2_950));
        assert_eq!(res.to_character_ids, vec![r_ok]);
        assert_eq!(res.stored_recipient_labels, vec![(r_ok, 1)]);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&sender_id].balance, IskCents::from_isk(2_050));
            assert!(snap.read_mails.contains(&(sender_id, res.mail_id)));
            assert_eq!(snap.mail_timestamps.get(&res.mail_id), Some(&1_700_000_123));
            assert_eq!(snap.mail_assigned_labels[&(sender_id, res.mail_id)], vec![2]);
            assert_eq!(snap.mail_assigned_labels[&(r_ok, res.mail_id)], vec![1]);
            assert!(snap.wallet_journal.iter().any(|j| {
                j.character_id == sender_id
                    && j.entry_type == "cspa"
                    && j.amount == IskCents::from_isk(-2_950)
            }));
        });
    }

    #[test]
    fn cspa_and_send_mail_edge_boundaries_utf8_org_routing_deleted_quota_and_fallbacks() {
        let ctx = shared_test_ctx();
        let caller_id = 93_100_001i64;
        let unrelated_no_ally = 93_100_002i64;
        let esi_blocked_target = 93_100_003i64;
        let esi_eq_boundary = 93_100_004i64;
        let social_blocked_only = 93_100_005i64;
        let neg_charge_target = 93_100_006i64;
        let ml_owner_id = 93_100_007i64;
        let ally_exec_ceo = 93_100_008i64;
        let ally_peer_id = 93_100_009i64;
        let non_char_target_id = 93_100_999i64; // not seeded in snap.characters

        let corp_sender = 98_210_001i64;
        let corp_other = 98_210_002i64;
        let corp_exec = 98_210_003i64;
        let corp_ally_peer = 98_210_004i64;
        let alliance_id = 99_210_001i64;
        let list_snap_only_id = 555_101i64;

        ctx.db.with_mut_snapshot(|snap| {
            // Two unrelated chars BOTH with alliance_id = None -> exempt_on_same_alliance must NOT exempt!
            seed_char(snap, make_char(caller_id, 1, corp_sender, None, 100_000));
            seed_char(snap, make_char(unrelated_no_ally, 2, corp_other, None, 0));
            seed_char(snap, make_char(esi_blocked_target, 3, corp_other, None, 0));
            seed_char(snap, make_char(esi_eq_boundary, 4, corp_other, None, 0));
            seed_char(snap, make_char(social_blocked_only, 5, corp_other, None, 0));
            seed_char(snap, make_char(neg_charge_target, 6, corp_other, None, 0));
            seed_char(snap, make_char(ml_owner_id, 7, corp_other, None, 1_000));
            seed_char(
                snap,
                make_char(ally_exec_ceo, 8, corp_exec, Some(alliance_id), 10_000),
            );
            seed_char(
                snap,
                make_char(ally_peer_id, 9, corp_ally_peer, Some(alliance_id), 10_000),
            );

            snap.corporations.insert(
                corp_sender,
                make_corp(corp_sender, caller_id, None, &[caller_id]),
            );
            snap.corporations.insert(
                corp_other,
                make_corp(corp_other, ml_owner_id, None, &[ml_owner_id, unrelated_no_ally]),
            );
            snap.corporations.insert(
                corp_exec,
                make_corp(corp_exec, ally_exec_ceo, Some(alliance_id), &[ally_exec_ceo]),
            );
            let ally_peer_ceo = ally_peer_id + 100;
            seed_char(
                snap,
                make_char(ally_peer_ceo, 10, corp_ally_peer, Some(alliance_id), 10_000),
            );
            snap.corporations.insert(
                corp_ally_peer,
                make_corp(
                    corp_ally_peer,
                    ally_peer_ceo,
                    Some(alliance_id),
                    &[ally_peer_ceo, ally_peer_id],
                ),
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Edge Alliance".into(),
                    short_name: "EDGE".into(),
                    executor_corp_id: corp_exec,
                    member_corp_ids: HashSet::from([corp_exec, corp_ally_peer]),
                },
            );

            // Negative stored CSPA charge contributes 0 (not subtract).
            snap.character_cspa_charges
                .insert(neg_charge_target, IskCents(-500));
            snap.character_cspa_charges
                .insert(unrelated_no_ally, IskCents::from_isk(1_000));
        });

        {
            let mut esi = ctx.esi_state.char_contacts.write().unwrap();
            esi.insert(
                esi_blocked_target,
                vec![EsiContactEntry {
                    contact_id: caller_id,
                    contact_type: "character".into(),
                    standing: 10.0,
                    is_watched: false,
                    is_blocked: true,
                    label_ids: Vec::new(),
                }],
            );
            esi.insert(
                esi_eq_boundary,
                vec![EsiContactEntry {
                    contact_id: caller_id,
                    contact_type: "character".into(),
                    standing: 2.0,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: Vec::new(),
                }],
            );
        }
        {
            let mut se = ctx.social_extras.lock().unwrap();
            se.contacts
                .entry(social_blocked_only)
                .or_default()
                .insert(caller_id, (10.0, false, 0));
            se.blocked
                .entry(social_blocked_only)
                .or_default()
                .insert(caller_id);
        }

        // Missing caller fails charge_cspa_action too.
        assert_eq!(
            charge_cspa_action(
                &ctx,
                &CspaCalculationInput {
                    caller_id: 999_888_777,
                    target_ids: &[unrelated_no_ally],
                    default_charge: IskCents::from_isk(500),
                    exempt_on_same_alliance: true,
                    exempt_min_standing: 0.0,
                }
            )
            .unwrap_err()
            .code,
            "CharacterNotFound"
        );

        // unrelated_no_ally (1_000) + esi_blocked (500) + esi_eq_boundary at min_standing=2.0 (500)
        // + social_blocked_only (500) + unknown positive non_char_target_id (500) + neg_charge_target (0) = 3_000 ISK
        let fee = calculate_cspa_fee(
            &ctx,
            &CspaCalculationInput {
                caller_id,
                target_ids: &[
                    unrelated_no_ally,
                    esi_blocked_target,
                    esi_eq_boundary,
                    social_blocked_only,
                    non_char_target_id,
                    neg_charge_target,
                ],
                default_charge: IskCents::from_isk(500),
                exempt_on_same_alliance: true,
                exempt_min_standing: 2.0,
            },
        )
        .unwrap();
        assert_eq!(fee, IskCents::from_isk(3_000));

        // esi_contacts blocked also blocks send_mail.
        assert_eq!(
            send_mail(
                &ctx,
                SendMailInput {
                    sender_id: caller_id,
                    recipients: vec![MailRecipientSpec {
                        recipient_id: esi_blocked_target,
                        kind: MailRecipientKind::Character,
                    }],
                    to_list_id: None,
                    to_corp_or_alliance_id: None,
                    subject: "hi".into(),
                    body: "there".into(),
                    approved_cost_isk: None,
                    max_subject_chars: 10,
                    max_body_chars: 10,
                    expand_org_recipients_in_db: true,
                    record_cspa_journal: false,
                    cspa_exempt_min_standing: 0.0,
                    cspa_exempt_same_corp: true,
                    now_unix: 1_700_000_000,
                }
            )
            .unwrap_err()
            .code,
            "ContactBlocked"
        );

        // Exact UTF-8 char limit boundary (`10` chars of 2-byte `é`, body `5` chars `á`), self-mail dedup & no journal when record_cspa_journal=false.
        let utf8_sub = "é".repeat(10);
        let utf8_body = "á".repeat(5);
        assert!(utf8_sub.len() > 10 && utf8_sub.chars().count() == 10);
        let self_and_paid = send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: vec![
                    MailRecipientSpec {
                        recipient_id: caller_id,
                        kind: MailRecipientKind::Character,
                    },
                    MailRecipientSpec {
                        recipient_id: unrelated_no_ally,
                        kind: MailRecipientKind::Character,
                    },
                    MailRecipientSpec {
                        recipient_id: unrelated_no_ally, // duplicate deduped!
                        kind: MailRecipientKind::Character,
                    },
                ],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: utf8_sub,
                body: utf8_body,
                approved_cost_isk: Some(1_000), // exact approved_cost boundary passes!
                max_subject_chars: 10,
                max_body_chars: 5,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_010,
            },
        )
        .unwrap();
        assert_eq!(self_and_paid.cspa_charged, IskCents::from_isk(1_000));
        assert_eq!(self_and_paid.stored_recipient_labels, vec![(unrelated_no_ally, 1)]);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.mails
                    .iter()
                    .filter(|m| m.message_id == self_and_paid.mail_id)
                    .count(),
                2
            );
            assert!(snap.wallet_journal.iter().all(|j| j.character_id != caller_id));
        });

        // Deleted mails don't count toward 1,000 quota: 999 active + 5 deleted succeeds!
        ctx.db.with_mut_snapshot(|snap| {
            for i in 1..=1_004i64 {
                let mid = 200_000 + i;
                snap.mails.push(MailRecord {
                    message_id: mid,
                    sender_id: caller_id,
                    recipient_id: unrelated_no_ally,
                    subject: "old".into(),
                    body: "old".into(),
                });
                if i <= 5 {
                    snap.deleted_mails.insert((unrelated_no_ally, mid));
                }
            }
            // Wait: unrelated_no_ally now has 999 non-deleted from above + 1 earlier = 1_000 -> delete one more so active == 999!
            snap.deleted_mails.insert((unrelated_no_ally, self_and_paid.mail_id));
        });
        assert!(send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: vec![
                    MailRecipientSpec {
                        recipient_id: unrelated_no_ally,
                        kind: MailRecipientKind::Character,
                    },
                    MailRecipientSpec {
                        recipient_id: neg_charge_target,
                        kind: MailRecipientKind::Character,
                    },
                ],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: "UnderQuota".into(),
                body: "999 active".into(),
                approved_cost_isk: Some(1_000),
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_020,
            }
        )
        .is_ok());

        // Mailing list seeded ONLY in snap.mailing_lists, owner bypasses ACL & negative cost clamped to 0, corp entity_access grants sender access, expand=false stores list row.
        ctx.db.with_mut_snapshot(|snap| {
            let mut ea = HashMap::new();
            ea.insert(corp_sender, 1i64);
            let mut members = HashMap::new();
            members.insert(caller_id, 1i64);
            members.insert(neg_charge_target, 1i64);
            snap.mailing_lists.insert(
                list_snap_only_id,
                MailingListRecord {
                    list_id: list_snap_only_id,
                    name: "Snap Only".into(),
                    owner_id: ml_owner_id,
                    default_access: 0,
                    default_role: 1,
                    cost: -500, // negative cost clamped to 0!
                    members,
                    entity_access: ea,
                    welcome_mail: None,
                    mail_ids: HashSet::new(),
                },
            );
        });
        // Owner can post even though not in members/entity_access; to_list_id=None still discovers MailingList from recipients.
        let ml_owner_res = send_mail(
            &ctx,
            SendMailInput {
                sender_id: ml_owner_id,
                recipients: vec![MailRecipientSpec {
                    recipient_id: list_snap_only_id,
                    kind: MailRecipientKind::MailingList,
                }],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: "Owner ML".into(),
                body: "free".into(),
                approved_cost_isk: Some(0),
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: false,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_030,
            },
        )
        .unwrap();
        assert_eq!(ml_owner_res.to_list_id, Some(list_snap_only_id));
        ctx.db.with_snapshot(|snap| {
            assert!(snap
                .mails
                .iter()
                .any(|m| m.message_id == ml_owner_res.mail_id && m.recipient_id == list_snap_only_id));
        });
        // Non-owner subscriber inherits corp entity_access and sends via explicit to_list_id without MailingList in recipients.
        assert!(send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: Vec::new(),
                to_list_id: Some(list_snap_only_id),
                to_corp_or_alliance_id: None,
                subject: "Corp Entity ACL".into(),
                body: "via to_list_id".into(),
                approved_cost_isk: Some(0),
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_035,
            }
        )
        .is_ok());

        // Unknown corp/alliance -> NotFound HTTP 400; executor restrict_corp_mails restricts peer corp member; Character fallback expands corp/alliance or checks CharacterNotFound when expand=false.
        let err_corp = send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: vec![MailRecipientSpec {
                    recipient_id: 98_999_777,
                    kind: MailRecipientKind::Corporation,
                }],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: "x".into(),
                body: "y".into(),
                approved_cost_isk: None,
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_040,
            },
        )
        .unwrap_err();
        assert_eq!(err_corp.code, "CorporationNotFound");
        assert_eq!(err_corp.http_status(), 400);

        let err_ally = send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: vec![MailRecipientSpec {
                    recipient_id: 99_999_777,
                    kind: MailRecipientKind::Alliance,
                }],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: "x".into(),
                body: "y".into(),
                approved_cost_isk: None,
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_040,
            },
        )
        .unwrap_err();
        assert_eq!(err_ally.code, "AllianceNotFound");
        assert_eq!(err_ally.http_status(), 400);

        ctx.db.with_mut_snapshot(|snap| {
            snap.corp_governance.insert(
                corp_exec,
                CorpGovState {
                    restrict_corp_mails: true,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            send_mail(
                &ctx,
                SendMailInput {
                    sender_id: ally_peer_id,
                    recipients: vec![MailRecipientSpec {
                        recipient_id: alliance_id,
                        kind: MailRecipientKind::Alliance,
                    }],
                    to_list_id: None,
                    to_corp_or_alliance_id: None,
                    subject: "exec restricts".into(),
                    body: "y".into(),
                    approved_cost_isk: None,
                    max_subject_chars: 100,
                    max_body_chars: 1_000,
                    expand_org_recipients_in_db: true,
                    record_cspa_journal: false,
                    cspa_exempt_min_standing: 0.0,
                    cspa_exempt_same_corp: true,
                    now_unix: 1_700_000_045,
                }
            )
            .unwrap_err()
            .code,
            "CorpMailRestricted"
        );
        // Executor CEO sends via empty recipients + to_corp_or_alliance_id fallback with expand=false -> stores (alliance_id, 8).
        let ally_fallback = send_mail(
            &ctx,
            SendMailInput {
                sender_id: ally_exec_ceo,
                recipients: Vec::new(),
                to_list_id: None,
                to_corp_or_alliance_id: Some(alliance_id),
                subject: "Ally fallback".into(),
                body: "y".into(),
                approved_cost_isk: None,
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: false,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_050,
            },
        )
        .unwrap();
        assert_eq!(ally_fallback.to_corp_or_alliance_id, Some(alliance_id));
        ctx.db.with_snapshot(|snap| {
            assert!(snap
                .mails
                .iter()
                .any(|m| m.message_id == ally_fallback.mail_id && m.recipient_id == alliance_id));
        });
        // Passing unknown character with expand=false errors CharacterNotFound; passing corp_sender with expand=true inside Character routes to handle_corp.
        assert_eq!(
            send_mail(
                &ctx,
                SendMailInput {
                    sender_id: caller_id,
                    recipients: vec![MailRecipientSpec {
                        recipient_id: non_char_target_id,
                        kind: MailRecipientKind::Character,
                    }],
                    to_list_id: None,
                    to_corp_or_alliance_id: None,
                    subject: "missing".into(),
                    body: "char".into(),
                    approved_cost_isk: None,
                    max_subject_chars: 100,
                    max_body_chars: 1_000,
                    expand_org_recipients_in_db: false,
                    record_cspa_journal: false,
                    cspa_exempt_min_standing: 0.0,
                    cspa_exempt_same_corp: true,
                    now_unix: 1_700_000_055,
                }
            )
            .unwrap_err()
            .code,
            "CharacterNotFound"
        );
        let char_as_corp = send_mail(
            &ctx,
            SendMailInput {
                sender_id: caller_id,
                recipients: vec![MailRecipientSpec {
                    recipient_id: corp_sender,
                    kind: MailRecipientKind::Character,
                }],
                to_list_id: None,
                to_corp_or_alliance_id: None,
                subject: "char->corp".into(),
                body: "routed".into(),
                approved_cost_isk: None,
                max_subject_chars: 100,
                max_body_chars: 1_000,
                expand_org_recipients_in_db: true,
                record_cspa_journal: false,
                cspa_exempt_min_standing: 0.0,
                cspa_exempt_same_corp: true,
                now_unix: 1_700_000_060,
            },
        )
        .unwrap();
        assert_eq!(char_as_corp.notify_recipient_ids, vec![caller_id]);
    }
}

// ---------------------------------------------------------------------------
// D4: market_acl
// ---------------------------------------------------------------------------
mod market_acl_tests {
    use super::*;
    use crate::domain::market_acl::{
        can_caller_view_market_location, can_caller_view_market_location_at,
        can_caller_view_structure_acl, character_has_structure_acl_access, contract_in_region,
        is_character_party_to_contract, is_contract_visible_to_character,
        is_public_available_contract, primary_online_industry_activity,
        verify_structure_market_query_access,
    };

    fn make_fortizar(structure_id: i64, owner_corp_id: i64, solar_system_id: u32) -> UpwellStructureRecord {
        UpwellStructureRecord {
            structure_id,
            type_id: 35_833, // Fortizar
            owner_corp_id,
            solar_system_id,
            name: format!("Fortizar {structure_id}"),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".to_string(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 1.0,
            armor_hp: 1.0,
            hull_hp: 1.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        }
    }

    #[test]
    fn character_has_structure_acl_access_handles_destroyed_deny_list_corp_alliance_standings_and_wars() {
        let structure_id = 1_000_000_000_101i64;
        let owner_corp = 98_300_001i64;
        let same_corp_char = 94_000_001i64;
        let same_ally_char = 94_000_002i64;
        let neutral_char = 94_000_003i64;
        let ally_corp = 98_300_002i64;
        let neutral_corp = 98_300_003i64;
        let alliance_id = 99_300_001i64;

        let mut snap = WorldStateSnapshot::default();
        seed_char(&mut snap, make_char(same_corp_char, 1, owner_corp, Some(alliance_id), 0));
        seed_char(&mut snap, make_char(same_ally_char, 2, ally_corp, Some(alliance_id), 0));
        seed_char(&mut snap, make_char(neutral_char, 3, neutral_corp, None, 0));
        snap.corporations
            .insert(owner_corp, make_corp(owner_corp, same_corp_char, Some(alliance_id), &[same_corp_char]));
        snap.corporations
            .insert(ally_corp, make_corp(ally_corp, same_ally_char, Some(alliance_id), &[same_ally_char]));
        snap.corporations
            .insert(neutral_corp, make_corp(neutral_corp, neutral_char, None, &[neutral_char]));

        let mut st = make_fortizar(structure_id, owner_corp, 30_000_142);
        snap.structures.insert(structure_id, st.clone());

        // Missing char -> false.
        assert!(!character_has_structure_acl_access(&snap, 999_111_222, &st));

        // Same corp & same alliance & neutral -> true by default.
        assert!(character_has_structure_acl_access(&snap, same_corp_char, &st));
        assert!(character_has_structure_acl_access(&snap, same_ally_char, &st));
        assert!(character_has_structure_acl_access(&snap, neutral_char, &st));

        // Destroyed -> false even for owner corp.
        st.state = "Destroyed".to_string();
        assert!(!character_has_structure_acl_access(&snap, same_corp_char, &st));
        st.state = "ShieldVulnerable".to_string();

        // Explicit deny list -> false.
        snap.structure_docking_acl_denied
            .entry(structure_id)
            .or_default()
            .insert(neutral_char);
        assert!(!character_has_structure_acl_access(&snap, neutral_char, &st));
        snap.structure_docking_acl_denied.clear();

        // Negative standing toward owner corp -> false; neutral 0.0 -> true.
        snap.characters
            .get_mut(&neutral_char)
            .unwrap()
            .standings
            .insert(owner_corp, -0.01);
        assert!(!character_has_structure_acl_access(&snap, neutral_char, &st));
        snap.characters
            .get_mut(&neutral_char)
            .unwrap()
            .standings
            .insert(owner_corp, 0.0);
        assert!(character_has_structure_acl_access(&snap, neutral_char, &st));

        // Active war in either direction denies access; inactive war allows access.
        snap.wars.insert(
            1,
            WarRecord {
                war_id: 1,
                aggressor_corp_id: neutral_corp,
                defender_corp_id: owner_corp,
                mutual: false,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
        assert!(!character_has_structure_acl_access(&snap, neutral_char, &st));
        snap.wars.get_mut(&1).unwrap().active = false;
        assert!(character_has_structure_acl_access(&snap, neutral_char, &st));

        snap.wars.insert(
            2,
            WarRecord {
                war_id: 2,
                aggressor_corp_id: owner_corp,
                defender_corp_id: neutral_corp,
                mutual: false,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
        assert!(!character_has_structure_acl_access(&snap, neutral_char, &st));
    }

    #[test]
    fn market_location_visibility_and_verify_structure_market_query_access_cover_stations_and_structures() {
        let ctx = shared_test_ctx();
        let owner_char = 94_000_010i64;
        let denied_char = 94_000_011i64;
        let owner_corp = 98_300_010i64;
        let other_corp = 98_300_011i64;
        let structure_id = 1_000_000_000_202i64;
        let market_item_id = 1_000_000_000_203i64;
        let station_id = 60_003_760i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(owner_char, 1, owner_corp, None, 0));
            seed_char(snap, make_char(denied_char, 2, other_corp, None, 0));
            snap.corporations
                .insert(owner_corp, make_corp(owner_corp, owner_char, None, &[owner_char]));
            snap.corporations
                .insert(other_corp, make_corp(other_corp, denied_char, None, &[denied_char]));

            // Non-structure (station / unknown) queries with require_structure_exists=false succeed.
            assert!(can_caller_view_structure_acl(snap, owner_char, station_id, None));
            assert!(can_caller_view_market_location(snap, denied_char, station_id));
            assert!(
                verify_structure_market_query_access(
                    Some(&ctx),
                    snap,
                    owner_char,
                    station_id,
                    false
                )
                .is_ok()
            );

            // Missing structure with require_structure_exists=true returns StructureNotFound.
            let err_missing = verify_structure_market_query_access(
                Some(&ctx),
                snap,
                owner_char,
                structure_id,
                true,
            )
            .unwrap_err();
            assert_eq!(err_missing.kind, DomainErrorKind::NotFound);
            assert_eq!(err_missing.code, "StructureNotFound");

            // Insert Fortizar WITHOUT market service -> denied.
            snap.structures
                .insert(structure_id, make_fortizar(structure_id, owner_corp, 30_000_142));
            assert!(!can_caller_view_market_location(snap, owner_char, structure_id));
            let err_no_svc = verify_structure_market_query_access(
                Some(&ctx),
                snap,
                owner_char,
                structure_id,
                true,
            )
            .unwrap_err();
            assert_eq!(err_no_svc.kind, DomainErrorKind::Forbidden);
            assert_eq!(err_no_svc.code, "StructureServiceAccessDenied");

            // Add Standup Market Hub (35892), initially offline via module_online_states=false -> still denied.
            snap.items.insert(
                market_item_id,
                InventoryItemRecord {
                    item_id: market_item_id,
                    type_id: 35_892,
                    owner_id: owner_corp,
                    location_id: structure_id,
                    flag: InventoryFlag::ServiceSlot0,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(market_item_id, false);
            assert!(!can_caller_view_market_location(snap, owner_char, structure_id));

            // Online Market Hub + deny list on denied_char -> owner passes, denied_char blocked.
            snap.module_online_states.insert(market_item_id, true);
            snap.structure_docking_acl_denied
                .entry(structure_id)
                .or_default()
                .insert(denied_char);

            assert!(can_caller_view_structure_acl(snap, owner_char, structure_id, Some(1_700_000_000)));
            assert!(!can_caller_view_structure_acl(snap, denied_char, structure_id, Some(1_700_000_000)));
            assert!(can_caller_view_market_location(snap, owner_char, structure_id));
            assert!(can_caller_view_market_location_at(
                Some(&ctx),
                snap,
                owner_char,
                structure_id,
                Some(1_700_000_000)
            ));
            assert!(!can_caller_view_market_location(snap, denied_char, structure_id));
            assert!(
                verify_structure_market_query_access(
                    Some(&ctx),
                    snap,
                    owner_char,
                    structure_id,
                    true
                )
                .is_ok()
            );
            let err_denied = verify_structure_market_query_access(
                Some(&ctx),
                snap,
                denied_char,
                structure_id,
                true,
            )
            .unwrap_err();
            assert_eq!(err_denied.kind, DomainErrorKind::Forbidden);
            assert_eq!(err_denied.code, "StructureServiceAccessDenied");
        });
    }

    #[test]
    fn primary_online_industry_activity_detects_manufacturing_invention_reactions_and_skips_offline_or_destroyed() {
        let structure_id = 1_000_000_000_303i64;
        let owner_corp = 98_300_020i64;
        let mut snap = WorldStateSnapshot::default();
        let mut st = make_fortizar(structure_id, owner_corp, 30_000_142);
        snap.structures.insert(structure_id, st.clone());

        // No services -> None.
        assert_eq!(primary_online_industry_activity(&snap, &st), None);

        // Add Manufacturing (35878), Invention (35886), and Reactions (45537).
        snap.items.insert(
            900_001,
            InventoryItemRecord {
                item_id: 900_001,
                type_id: 35_878,
                owner_id: owner_corp,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot0,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_002,
            InventoryItemRecord {
                item_id: 900_002,
                type_id: 35_886,
                owner_id: owner_corp,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_003,
            InventoryItemRecord {
                item_id: 900_003,
                type_id: 45_537,
                owner_id: owner_corp,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot2,
                singleton: true,
                ..Default::default()
            },
        );

        // Priority picks Manufacturing (1) first.
        assert_eq!(primary_online_industry_activity(&snap, &st), Some(1));

        // Offline Manufacturing reveals Invention (8) (actually Research/Copy shares 3..=5? Check 35886 -> activity 8).
        snap.module_online_states.insert(900_001, false);
        assert_eq!(primary_online_industry_activity(&snap, &st), Some(8));

        // Offline Invention reveals Reactions (9).
        snap.module_online_states.insert(900_002, false);
        assert_eq!(primary_online_industry_activity(&snap, &st), Some(9));

        // Offline Reactions -> None.
        snap.module_online_states.insert(900_003, false);
        assert_eq!(primary_online_industry_activity(&snap, &st), None);

        // Re-enable Manufacturing, then Destroy structure -> None.
        snap.module_online_states.insert(900_001, true);
        st.state = "Destroyed".to_string();
        assert_eq!(primary_online_industry_activity(&snap, &st), None);
    }

    #[test]
    fn contract_visibility_and_region_helpers_cover_public_private_parties_and_locations() {
        let ctx = shared_test_ctx();
        let issuer_id = 94_000_020i64;
        let acceptor_id = 94_000_021i64;
        let bidder_id = 94_000_022i64;
        let corp_mate_id = 94_000_023i64;
        let ally_mate_id = 94_000_024i64;
        let stranger_id = 94_000_025i64;

        let issuer_corp = 98_300_030i64;
        let target_corp = 98_300_031i64;
        let alliance_id = 99_300_030i64;
        let other_corp = 98_300_032i64;

        let public_c = ContractRecord {
            contract_id: 1,
            issuer_id,
            availability: "Public".to_string(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_public_available_contract(&public_c));
        assert!(is_contract_visible_to_character(
            &public_c,
            stranger_id,
            other_corp,
            None
        ));

        let private_corp_c = ContractRecord {
            contract_id: 2,
            issuer_id,
            acceptor_id: Some(acceptor_id),
            availability: "Private".to_string(),
            assignee_id: target_corp,
            for_corp: true,
            current_bidder_id: Some(bidder_id),
            ..Default::default()
        };
        assert!(!is_public_available_contract(&private_corp_c));
        assert!(is_contract_visible_to_character(
            &private_corp_c,
            issuer_id,
            issuer_corp,
            Some(alliance_id)
        ));
        assert!(!is_contract_visible_to_character(
            &private_corp_c,
            acceptor_id,
            other_corp,
            None
        ));
        assert!(is_contract_visible_to_character(
            &private_corp_c,
            corp_mate_id,
            target_corp,
            None
        ));

        let corp_avail_c = ContractRecord {
            contract_id: 21,
            issuer_id,
            issuer_corp_id: issuer_corp,
            availability: "Corporation".to_string(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(
            &corp_avail_c,
            stranger_id,
            issuer_corp,
            None
        ));
        assert!(!is_contract_visible_to_character(
            &corp_avail_c,
            stranger_id,
            other_corp,
            None
        ));
        assert!(!is_contract_visible_to_character(
            &private_corp_c,
            stranger_id,
            other_corp,
            None
        ));

        let private_ally_c = ContractRecord {
            contract_id: 3,
            issuer_id,
            availability: "Private".to_string(),
            assignee_id: alliance_id,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(
            &private_ally_c,
            ally_mate_id,
            other_corp,
            Some(alliance_id)
        ));
        assert!(!is_contract_visible_to_character(
            &private_ally_c,
            stranger_id,
            other_corp,
            None
        ));

        let snap = WorldStateSnapshot::default();
        assert!(!is_character_party_to_contract(&snap, issuer_id, &private_corp_c)); // for_corp=true!
        assert!(is_character_party_to_contract(&snap, acceptor_id, &private_corp_c));
        assert!(is_character_party_to_contract(&snap, bidder_id, &private_corp_c));
        assert!(is_character_party_to_contract(&snap, issuer_id, &private_ally_c)); // for_corp=false
        assert!(!is_character_party_to_contract(&snap, stranger_id, &private_ally_c));

        // Region check for NPC station (Jita 4-4 = 60_003_760 in The Forge = 10_000_002) & Upwell structure.
        let mut reg_snap = WorldStateSnapshot::default();
        let st_id = 1_000_000_000_404i64;
        reg_snap
            .structures
            .insert(st_id, make_fortizar(st_id, issuer_corp, 30_000_142));
        assert!(contract_in_region(&ctx.sde, &reg_snap, 60_003_760, 10_000_002));
        assert!(!contract_in_region(&ctx.sde, &reg_snap, 60_003_760, 10_000_043));
        assert!(contract_in_region(&ctx.sde, &reg_snap, st_id, 10_000_002));
        assert!(!contract_in_region(&ctx.sde, &reg_snap, st_id, 10_000_043));
        assert!(!contract_in_region(&ctx.sde, &reg_snap, 9_999_999_999, 10_000_002));
    }

    #[test]
    fn market_acl_and_structure_visibility_exhaustive_branch_and_operator_mutants() {
        let ctx = shared_test_ctx();
        let owner_corp = 98_301_001i64;
        let other_corp = 98_301_002i64;
        let war_x = 98_301_003i64;
        let war_y = 98_301_004i64;
        let npc_corp = 1_000_001i64;
        let ally_a = 99_301_001i64;
        let ally_b = 99_301_002i64;

        let owner_char = 94_010_001i64;
        let same_ally_bad_standing = 94_010_002i64;
        let diff_ally_bad_standing = 94_010_003i64;
        let no_ally_bad_standing = 94_010_004i64;
        let neutral_char = 94_010_005i64;
        let unknown_char = 94_999_888i64;

        ctx.db.with_mut_snapshot(|snap| {
            // 1. character_has_structure_acl_access subtle corp, alliance, and war operand combinations
            seed_char(snap, make_char(owner_char, 1, owner_corp, Some(ally_a), 0));
            seed_char(snap, make_char(same_ally_bad_standing, 2, other_corp, Some(ally_a), 0));
            seed_char(snap, make_char(diff_ally_bad_standing, 3, war_x, Some(ally_b), 0));
            seed_char(snap, make_char(no_ally_bad_standing, 4, other_corp, None, 0));
            seed_char(snap, make_char(neutral_char, 5, other_corp, None, 0));

            snap.characters
                .get_mut(&owner_char)
                .unwrap()
                .standings
                .insert(owner_corp, -5.0);
            snap.characters
                .get_mut(&same_ally_bad_standing)
                .unwrap()
                .standings
                .insert(owner_corp, -5.0);
            snap.characters
                .get_mut(&diff_ally_bad_standing)
                .unwrap()
                .standings
                .insert(owner_corp, -5.0);
            snap.characters
                .get_mut(&no_ally_bad_standing)
                .unwrap()
                .standings
                .insert(owner_corp, -5.0);

            snap.corporations
                .insert(owner_corp, make_corp(owner_corp, owner_char, Some(ally_a), &[owner_char]));
            snap.corporations
                .insert(other_corp, make_corp(other_corp, neutral_char, None, &[neutral_char]));

            // Unrelated active war + partial-corp active wars must NOT block neutral_char vs owner_corp
            snap.wars.insert(
                101,
                WarRecord {
                    war_id: 101,
                    aggressor_corp_id: war_x,
                    defender_corp_id: war_y,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            snap.wars.insert(
                102,
                WarRecord {
                    war_id: 102,
                    aggressor_corp_id: other_corp,
                    defender_corp_id: war_x,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            snap.wars.insert(
                103,
                WarRecord {
                    war_id: 103,
                    aggressor_corp_id: war_x,
                    defender_corp_id: owner_corp,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            snap.wars.insert(
                104,
                WarRecord {
                    war_id: 104,
                    aggressor_corp_id: owner_corp,
                    defender_corp_id: war_y,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            snap.wars.insert(
                105,
                WarRecord {
                    war_id: 105,
                    aggressor_corp_id: war_y,
                    defender_corp_id: other_corp,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );

            let player_st_id = 1_000_000_005_001i64;
            let player_st = make_fortizar(player_st_id, owner_corp, 30_000_142);
            snap.structures.insert(player_st_id, player_st.clone());

            // Same corp overrides negative standing -> true (`!=` fails when standing < 0)
            assert!(character_has_structure_acl_access(snap, owner_char, &player_st));
            // Same alliance overrides negative standing -> true (`==` vs `!=`)
            assert!(character_has_structure_acl_access(snap, same_ally_bad_standing, &player_st));
            // Different alliance falls through to negative standing -> false
            assert!(!character_has_structure_acl_access(snap, diff_ally_bad_standing, &player_st));
            // Neither corp/alliance match nor negative standing; unrelated/partial wars do NOT block -> true
            snap.characters
                .get_mut(&neutral_char)
                .unwrap()
                .standings
                .insert(owner_corp, 2.5);
            assert!(character_has_structure_acl_access(snap, neutral_char, &player_st));

            // Corp without alliance + char without alliance + negative standing -> false (`&&` vs `||` on is_some)
            snap.corporations.get_mut(&owner_corp).unwrap().alliance_id = None;
            assert!(!character_has_structure_acl_access(snap, no_ally_bad_standing, &player_st));
            snap.corporations.get_mut(&owner_corp).unwrap().alliance_id = Some(ally_a);

            // 2. Player structure market hub + fuel=0 (`allows_market = false`) & Destroyed with online module
            let player_mkt_id = 1_000_000_005_002i64;
            snap.items.insert(
                player_mkt_id,
                InventoryItemRecord {
                    item_id: player_mkt_id,
                    type_id: 35_892,
                    owner_id: owner_corp,
                    location_id: player_st_id,
                    flag: InventoryFlag::ServiceSlot0,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(player_mkt_id, true);

            // Neutral char passes basic `character_has_structure_acl_access`, fails player-corp docking profile!
            assert!(!can_caller_view_structure_acl(snap, neutral_char, player_st_id, None));
            assert!(!can_caller_view_market_location_at(Some(&ctx), snap, neutral_char, player_st_id, None));
            assert!(!can_caller_view_structure_acl(snap, 0, player_st_id, None));

            // Owner fails when fuel_blocks == 0 (`structure_allows_market_orders == false`)
            snap.structures.get_mut(&player_st_id).unwrap().fuel_blocks = 0;
            assert!(!can_caller_view_market_location_at(Some(&ctx), snap, owner_char, player_st_id, None));
            let err_no_fuel = verify_structure_market_query_access(
                Some(&ctx),
                snap,
                owner_char,
                player_st_id,
                true,
            )
            .unwrap_err();
            assert_eq!(err_no_fuel.code, "StructureServiceAccessDenied");
            snap.structures.get_mut(&player_st_id).unwrap().fuel_blocks = 100;

            // Owner passes with online hub, fuel > 0, and clock now_unix
            assert!(can_caller_view_market_location_at(
                Some(&ctx),
                snap,
                owner_char,
                player_st_id,
                Some(1_700_000_000)
            ));
            assert!(verify_structure_market_query_access(
                Some(&ctx),
                snap,
                owner_char,
                player_st_id,
                true,
            )
            .is_ok());

            // Destroyed structure EVEN with online market module -> denied everywhere
            snap.structures.get_mut(&player_st_id).unwrap().state = "Destroyed".to_string();
            assert!(!can_caller_view_structure_acl(snap, owner_char, player_st_id, None));
            assert!(!can_caller_view_market_location_at(Some(&ctx), snap, owner_char, player_st_id, None));
            assert_eq!(
                verify_structure_market_query_access(Some(&ctx), snap, owner_char, player_st_id, true)
                    .unwrap_err()
                    .code,
                "StructureServiceAccessDenied"
            );

            // 3. Public-corp structure (`owner_corp < 98_000_000`, absent from `snap.corporations`)
            // exercises `caller_char_id <= 0` (`<=` vs `>`) and `!character_has_structure_acl_access`
            // with `unknown_char` missing from `snap.characters`.
            let pub_st_id = 1_000_000_005_010i64;
            let pub_mkt_id = 1_000_000_005_011i64;
            snap.structures
                .insert(pub_st_id, make_fortizar(pub_st_id, npc_corp, 30_000_142));
            snap.items.insert(
                pub_mkt_id,
                InventoryItemRecord {
                    item_id: pub_mkt_id,
                    type_id: 35_892,
                    owner_id: npc_corp,
                    location_id: pub_st_id,
                    flag: InventoryFlag::ServiceSlot0,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(pub_mkt_id, true);

            assert!(can_caller_view_structure_acl(snap, 0, pub_st_id, None));
            assert!(can_caller_view_structure_acl(snap, -1, pub_st_id, None));
            assert!(can_caller_view_market_location_at(None, snap, 0, pub_st_id, None));
            assert!(can_caller_view_market_location_at(None, snap, -1, pub_st_id, None));

            // Unknown positive char fails `character_has_structure_acl_access` even though docking profile passes
            assert!(!can_caller_view_structure_acl(snap, unknown_char, pub_st_id, None));
            assert!(!can_caller_view_market_location_at(None, snap, unknown_char, pub_st_id, None));
            assert!(can_caller_view_market_location_at(None, snap, neutral_char, pub_st_id, None));
        });
    }

    #[test]
    fn contract_visibility_and_party_exhaustive_operator_mutants() {
        let issuer = 94_020_001i64;
        let char_a = 94_020_002i64;
        let char_b = 94_020_003i64;
        let corp_issuer = 98_302_001i64;
        let corp_target = 98_302_002i64;
        let corp_other = 98_302_003i64;
        let ally_target = 99_302_001i64;
        let ally_other = 99_302_002i64;

        // is_public_available_contract truth table
        let empty_avail_unassigned = ContractRecord {
            availability: String::new(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_public_available_contract(&empty_avail_unassigned));
        assert!(is_public_available_contract(&ContractRecord {
            availability: "public".into(),
            assignee_id: 0,
            ..Default::default()
        }));
        assert!(!is_public_available_contract(&ContractRecord {
            availability: "Public".into(),
            assignee_id: char_a,
            ..Default::default()
        }));
        assert!(!is_public_available_contract(&ContractRecord {
            availability: "Private".into(),
            assignee_id: 0,
            ..Default::default()
        }));

        // is_contract_visible_to_character with assignee_id != 0
        let char_assigned = ContractRecord {
            issuer_id: issuer,
            availability: "Private".into(),
            assignee_id: char_a,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(&char_assigned, issuer, corp_other, None));
        assert!(is_contract_visible_to_character(&char_assigned, char_a, corp_other, None));
        assert!(!is_contract_visible_to_character(&char_assigned, char_b, corp_other, Some(ally_other)));

        let corp_assigned = ContractRecord {
            issuer_id: issuer,
            availability: "Private".into(),
            assignee_id: corp_target,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(&corp_assigned, char_b, corp_target, None));
        assert!(!is_contract_visible_to_character(&corp_assigned, char_b, 0, None));
        assert!(!is_contract_visible_to_character(&corp_assigned, char_b, corp_other, Some(ally_other)));

        let ally_assigned = ContractRecord {
            issuer_id: issuer,
            availability: "Private".into(),
            assignee_id: ally_target,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(&ally_assigned, char_b, corp_other, Some(ally_target)));
        assert!(!is_contract_visible_to_character(&ally_assigned, char_b, corp_other, Some(0)));
        assert!(!is_contract_visible_to_character(&ally_assigned, char_b, corp_other, Some(ally_other)));

        // is_contract_visible_to_character with assignee_id == 0 across every availability arm
        let corp_zero_issuer = ContractRecord {
            issuer_id: issuer,
            issuer_corp_id: 0,
            availability: "Corporation".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(!is_contract_visible_to_character(&corp_zero_issuer, char_b, 0, None));

        let ally_unassigned = ContractRecord {
            issuer_id: issuer,
            availability: "Alliance".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(!is_contract_visible_to_character(&ally_unassigned, char_b, corp_other, None));
        assert!(!is_contract_visible_to_character(&ally_unassigned, char_b, corp_other, Some(ally_target)));
        assert!(is_contract_visible_to_character(&ally_unassigned, char_b, corp_other, Some(0)));

        let personal_unassigned = ContractRecord {
            issuer_id: issuer,
            availability: "Personal".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(&personal_unassigned, issuer, corp_other, None));
        assert!(!is_contract_visible_to_character(&personal_unassigned, char_b, corp_other, None));

        let private_unassigned = ContractRecord {
            issuer_id: issuer,
            availability: "Private".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(!is_contract_visible_to_character(&private_unassigned, char_b, corp_other, None));

        let region_fallback = ContractRecord {
            issuer_id: issuer,
            availability: "Region".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_contract_visible_to_character(&region_fallback, char_b, corp_other, None));

        // is_character_party_to_contract covering bid_history, assignee_id == char_id, corp & alliance snapshot paths
        let mut snap = WorldStateSnapshot::default();
        let zero_corp_char = 94_020_010i64;
        let target_corp_char = 94_020_011i64;
        let issuer_corp_char = 94_020_012i64;
        let target_ally_char = 94_020_013i64;
        let other_ally_char = 94_020_014i64;
        let hist_bidder = 94_020_015i64;

        seed_char(&mut snap, make_char(zero_corp_char, 1, 0, Some(0), 0));
        seed_char(&mut snap, make_char(target_corp_char, 2, corp_target, None, 0));
        seed_char(&mut snap, make_char(issuer_corp_char, 3, corp_issuer, None, 0));
        seed_char(&mut snap, make_char(target_ally_char, 4, corp_other, Some(ally_target), 0));
        seed_char(&mut snap, make_char(other_ally_char, 5, corp_other, Some(ally_other), 0));
        seed_char(&mut snap, make_char(hist_bidder, 6, corp_other, None, 0));

        let rich_c = ContractRecord {
            contract_id: 500,
            issuer_id: issuer,
            issuer_corp_id: corp_issuer,
            for_corp: false,
            availability: "Private".into(),
            assignee_id: char_a,
            acceptor_id: Some(char_b),
            current_bidder_id: Some(94_020_099),
            bid_history: vec![(hist_bidder, IskCents::from_isk(100), 1_700_000_000)],
            ..Default::default()
        };
        assert!(is_character_party_to_contract(&snap, issuer, &rich_c));
        assert!(is_character_party_to_contract(&snap, char_a, &rich_c));
        assert!(is_character_party_to_contract(&snap, char_b, &rich_c));
        assert!(is_character_party_to_contract(&snap, 94_020_099, &rich_c));
        assert!(is_character_party_to_contract(&snap, hist_bidder, &rich_c));
        assert!(!is_character_party_to_contract(&snap, zero_corp_char, &rich_c));

        let zero_c = ContractRecord {
            contract_id: 501,
            issuer_id: issuer,
            issuer_corp_id: 0,
            for_corp: true,
            availability: "Private".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(!is_character_party_to_contract(&snap, zero_corp_char, &zero_c));

        let corp_assignee_c = ContractRecord {
            contract_id: 502,
            issuer_id: issuer,
            issuer_corp_id: corp_issuer,
            for_corp: true,
            availability: "Private".into(),
            assignee_id: corp_target,
            ..Default::default()
        };
        assert!(is_character_party_to_contract(&snap, target_corp_char, &corp_assignee_c));
        assert!(!is_character_party_to_contract(&snap, issuer_corp_char, &corp_assignee_c));

        let corp_avail_c = ContractRecord {
            contract_id: 503,
            issuer_id: issuer,
            issuer_corp_id: corp_issuer,
            for_corp: true,
            availability: "Corporation".into(),
            assignee_id: 0,
            ..Default::default()
        };
        assert!(is_character_party_to_contract(&snap, issuer_corp_char, &corp_avail_c));
        assert!(!is_character_party_to_contract(&snap, target_corp_char, &corp_avail_c));

        let ally_assignee_c = ContractRecord {
            contract_id: 504,
            issuer_id: issuer,
            issuer_corp_id: corp_issuer,
            for_corp: true,
            availability: "Private".into(),
            assignee_id: ally_target,
            ..Default::default()
        };
        assert!(is_character_party_to_contract(&snap, target_ally_char, &ally_assignee_c));
        assert!(!is_character_party_to_contract(&snap, other_ally_char, &ally_assignee_c));
    }
}

// ---------------------------------------------------------------------------
// D5: fleets
// ---------------------------------------------------------------------------
mod fleets_tests {
    use super::*;
    use crate::domain::fleets::{
        AcceptFleetInviteInput, InviteFleetMemberInput, MoveFleetMemberInput,
        UpdateFleetSettingsInput, accept_fleet_invite, can_act_on_member, can_invite_member,
        can_manage_squad, can_manage_wing, create_fleet_squad, create_fleet_wing,
        delete_fleet_squad, delete_fleet_wing, disband_fleet, ensure_fleet_channel_member,
        fleet_role_to_esi_str, get_character_fleet, get_fleet_settings, i64_to_role,
        invite_fleet_member, is_fleet_boss_or_fc, is_fleet_member, kick_or_remove_fleet_member,
        list_fleet_members, list_fleet_wings, member_effective_position, move_fleet_member,
        parse_esi_fleet_role, remove_character_from_fleet_domain, rename_fleet_squad,
        rename_fleet_wing, role_name_str, role_to_i64, squad_parent_wing_id,
        update_fleet_settings, validate_wing_and_squad,
    };
    use crate::services::fleet_social::fleet_social_fleets::FleetAdvertState;
    use crate::services::space_destiny::{clear_session_change_timer, record_session_change_timer};

    fn seed_basic_fleet(
        ctx: &ServerContext,
        fleet_id: i64,
        boss_id: i64,
        wing_id: i64,
        squad_id: i64,
        free_move: bool,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            let mut wings = HashMap::new();
            wings.insert(
                wing_id,
                FleetWingRecord {
                    wing_id,
                    name: "Wing 1".to_string(),
                    commander_id: None,
                    squad_ids: vec![squad_id],
                },
            );
            let mut squads = HashMap::new();
            squads.insert(
                squad_id,
                FleetSquadRecord {
                    squad_id,
                    wing_id,
                    name: "Squad 1".to_string(),
                    commander_id: None,
                },
            );
            let mut member_details = HashMap::new();
            member_details.insert(
                boss_id,
                FleetMemberRecord {
                    character_id: boss_id,
                    role: FleetRole::FleetCommander,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 2,
                    role_name: "Fleet Commander".to_string(),
                    join_time_unix: 1_700_000_000,
                },
            );
            snap.fleets.insert(
                fleet_id,
                FleetRecord {
                    fleet_id,
                    boss_id,
                    members: vec![boss_id],
                    wings: vec![wing_id],
                    squads: vec![squad_id],
                    free_move,
                    is_registered: false,
                    wing_details: wings,
                    squad_details: squads,
                    member_details,
                    invites: HashSet::new(),
                    motd: "Initial MOTD".to_string(),
                },
            );
        });
    }

    #[test]
    fn fleet_role_conversions_channel_member_and_hierarchy_predicates() {
        for (role, num, esi, name) in [
            (FleetRole::FleetCommander, 1, "fleet_commander", "Fleet Commander"),
            (FleetRole::WingCommander, 2, "wing_commander", "Wing Commander"),
            (FleetRole::SquadCommander, 3, "squad_commander", "Squad Commander"),
            (FleetRole::SquadMember, 4, "squad_member", "Squad Member"),
        ] {
            assert_eq!(role_to_i64(role), num);
            assert_eq!(i64_to_role(num), role);
            assert_eq!(role_name_str(role), name);
            assert_eq!(fleet_role_to_esi_str(role), (esi, name));
            assert_eq!(parse_esi_fleet_role(esi), Some(role));
        }
        assert_eq!(i64_to_role(99), FleetRole::SquadMember);
        assert_eq!(parse_esi_fleet_role("bogus"), None);

        // Chat channel helper creates channel once and preserves existing MOTD.
        let mut channels = HashMap::new();
        ensure_fleet_channel_member(&mut channels, 500, 10, 10);
        let key = "fleetid:500".to_string();
        channels.get_mut(&key).unwrap().motd = "Keep Me".to_string();
        ensure_fleet_channel_member(&mut channels, 500, 10, 20);
        assert_eq!(channels[&key].motd, "Keep Me");
        assert!(channels[&key].members.contains(&10));
        assert!(channels[&key].members.contains(&20));

        // Hierarchy predicates on a fleet with FC/Boss(1), Wing1(101)/Squad1(201), Squad2(202).
        let mut fleet = FleetRecord {
            fleet_id: 1,
            boss_id: 1,
            members: vec![1, 2, 3, 4],
            wings: vec![101, 102],
            squads: vec![201, 202],
            ..Default::default()
        };
        fleet.wing_details.insert(
            101,
            FleetWingRecord {
                wing_id: 101,
                name: "W1".to_string(),
                commander_id: Some(2),
                squad_ids: vec![201],
            },
        );
        fleet.wing_details.insert(
            102,
            FleetWingRecord {
                wing_id: 102,
                name: "W2".to_string(),
                commander_id: None,
                squad_ids: vec![202],
            },
        );
        // Deliberately keep squad 202 out of squad_details to test wing_details.squad_ids fallback!
        fleet.squad_details.insert(
            201,
            FleetSquadRecord {
                squad_id: 201,
                wing_id: 101,
                name: "S1".to_string(),
                commander_id: Some(3),
            },
        );
        fleet.member_details.insert(
            2,
            FleetMemberRecord {
                character_id: 2,
                role: FleetRole::WingCommander,
                wing_id: Some(101),
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 1,
            },
        );
        fleet.member_details.insert(
            3,
            FleetMemberRecord {
                character_id: 3,
                role: FleetRole::SquadCommander,
                wing_id: Some(101),
                squad_id: Some(201),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 1,
            },
        );
        fleet.member_details.insert(
            4,
            FleetMemberRecord {
                character_id: 4,
                role: FleetRole::SquadMember,
                wing_id: Some(101),
                squad_id: Some(201),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 1,
            },
        );

        assert!(is_fleet_member(&fleet, 1));
        assert!(is_fleet_member(&fleet, 4));
        assert!(!is_fleet_member(&fleet, 99));
        assert!(is_fleet_boss_or_fc(&fleet, 1));
        assert!(!is_fleet_boss_or_fc(&fleet, 2));

        assert_eq!(squad_parent_wing_id(&fleet, 201), Some(101));
        assert_eq!(squad_parent_wing_id(&fleet, 202), Some(102));
        assert_eq!(squad_parent_wing_id(&fleet, 999), None);

        assert!(can_manage_wing(&fleet, 1, 102));
        assert!(can_manage_wing(&fleet, 2, 101));
        assert!(!can_manage_wing(&fleet, 2, 102));
        assert!(can_manage_squad(&fleet, 2, 201));
        assert!(can_manage_squad(&fleet, 3, 201));
        assert!(!can_manage_squad(&fleet, 3, 202));
        assert!(!can_manage_squad(&fleet, 4, 201));

        assert_eq!(
            member_effective_position(&fleet, 1),
            (FleetRole::FleetCommander, None, None)
        );
        assert_eq!(
            member_effective_position(&fleet, 3),
            (FleetRole::SquadCommander, Some(101), Some(201))
        );

        // Authority checks: WC(2) acts on SquadMember(4) in wing 101; SM(4) cannot act on anyone.
        assert!(can_act_on_member(&fleet, 2, 4, None));
        assert!(!can_act_on_member(&fleet, 4, 3, None));
        assert!(can_invite_member(&fleet, 3, FleetRole::SquadMember, Some(101), Some(201)));
        assert!(!can_invite_member(&fleet, 3, FleetRole::WingCommander, Some(101), None));

        // validate_wing_and_squad resolves squad->wing or rejects mismatched wing/squad.
        assert_eq!(
            validate_wing_and_squad(&fleet, 1, FleetRole::SquadMember, Some(101), Some(201), false)
                .unwrap(),
            (Some(101), Some(201))
        );
        let err_mismatch = validate_wing_and_squad(
            &fleet,
            1,
            FleetRole::SquadMember,
            Some(102),
            Some(201),
            true,
        )
        .unwrap_err();
        assert_eq!(err_mismatch.kind, DomainErrorKind::InvalidInput);
        assert_eq!(err_mismatch.code, "InvalidArgument");
    }

    #[test]
    fn get_character_fleet_settings_list_members_and_wings_cover_errors_and_happy_paths() {
        let ctx = shared_test_ctx();
        let boss_id = 95_000_001i64;
        let wingman_id = 95_000_002i64;
        let outsider_id = 95_000_099i64;
        let fleet_id = 7_700_001i64;
        let wing_id = 7_700_101i64;
        let squad_id = 7_700_201i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(boss_id, 1, 98_400_001, None, 0));
            seed_char(snap, make_char(wingman_id, 2, 98_400_001, None, 0));
            seed_char(snap, make_char(outsider_id, 3, 98_400_001, None, 0));
        });
        seed_basic_fleet(&ctx, fleet_id, boss_id, wing_id, squad_id, false);
        ctx.db.with_mut_snapshot(|snap| {
            let f = snap.fleets.get_mut(&fleet_id).unwrap();
            f.members.push(wingman_id);
            f.member_details.insert(
                wingman_id,
                FleetMemberRecord {
                    character_id: wingman_id,
                    role: FleetRole::SquadMember,
                    wing_id: Some(wing_id),
                    squad_id: Some(squad_id),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Member".to_string(),
                    join_time_unix: 1_700_000_010,
                },
            );
        });

        assert_eq!(
            get_character_fleet(&ctx, outsider_id).unwrap_err().code,
            "NotInFleet"
        );
        let mem_view = get_character_fleet(&ctx, wingman_id).unwrap();
        assert_eq!(mem_view.fleet_id, fleet_id);
        assert_eq!(mem_view.wing_id, Some(wing_id));
        assert_eq!(mem_view.squad_id, Some(squad_id));

        assert_eq!(
            get_fleet_settings(&ctx, fleet_id, Some(outsider_id))
                .unwrap_err()
                .code,
            "NotInFleet"
        );
        let settings = get_fleet_settings(&ctx, fleet_id, Some(boss_id)).unwrap();
        assert_eq!(settings.motd, "Initial MOTD");
        assert!(!settings.is_free_move);

        // Empty update -> InvalidArgument.
        let err_empty = update_fleet_settings(
            &ctx,
            UpdateFleetSettingsInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                require_boss_only: false,
                motd: None,
                is_free_move: None,
                is_registered: None,
                is_voice_enabled: None,
                is_loot_logging: None,
            },
        )
        .unwrap_err();
        assert_eq!(err_empty.code, "InvalidArgument");

        // Subordinate cannot update settings.
        let err_perm = update_fleet_settings(
            &ctx,
            UpdateFleetSettingsInput {
                fleet_id,
                caller_char_id: Some(wingman_id),
                require_boss_only: true,
                motd: Some("Hacked".to_string()),
                is_free_move: None,
                is_registered: None,
                is_voice_enabled: None,
                is_loot_logging: None,
            },
        )
        .unwrap_err();
        assert_eq!(err_perm.code, "FleetPermissionDenied");

        let upd = update_fleet_settings(
            &ctx,
            UpdateFleetSettingsInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                require_boss_only: true,
                motd: Some("New MOTD".to_string()),
                is_free_move: Some(true),
                is_registered: None,
                is_voice_enabled: Some(false),
                is_loot_logging: None,
            },
        )
        .unwrap();
        assert!(!upd.old_free_move);
        assert!(upd.new_free_move);
        assert_eq!(upd.updated_motd.as_deref(), Some("New MOTD"));

        let members = list_fleet_members(&ctx, fleet_id, Some(boss_id)).unwrap();
        assert_eq!(members.len(), 2);
        let wings = list_fleet_wings(&ctx, fleet_id, Some(wingman_id)).unwrap();
        assert_eq!(wings.len(), 1);
        assert_eq!(wings[0].wing_id, wing_id);
        assert_eq!(wings[0].squads.len(), 1);
        assert_eq!(wings[0].squads[0].squad_id, squad_id);
    }

    #[test]
    fn invite_accept_and_move_fleet_member_enforce_online_lock_session_timer_and_free_move() {
        let ctx = shared_test_ctx();
        let boss_id = 95_000_101i64;
        let recruit_id = 95_000_102i64;
        let second_wing_char = 95_000_103i64;
        let fleet_id = 7_700_010i64;
        let wing_id = 7_700_110i64;
        let squad_id = 7_700_210i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(boss_id, 10, 98_400_010, None, 0));
            seed_char(snap, make_char(recruit_id, 1, 98_400_010, None, 0));
            seed_char(snap, make_char(second_wing_char, 12, 98_400_010, None, 0));
        });
        seed_basic_fleet(&ctx, fleet_id, boss_id, wing_id, squad_id, false);

        // 1. Offline recruit cannot be invited when enforce_online_and_capacity_checks=true.
        let err_offline = invite_fleet_member(
            &ctx,
            InviteFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                inviter_id_override: None,
                invitee_id: recruit_id,
                role: FleetRole::SquadMember,
                wing_id: Some(wing_id),
                squad_id: Some(squad_id),
                enforce_online_and_capacity_checks: true,
                validate_hierarchy_strict: true,
                now_unix: 1_700_000_000,
            },
        )
        .unwrap_err();
        assert_eq!(err_offline.code, "CharacterOffline");

        // Acquire online lock for recruit (using seeded demo account user_id = 1) and invite.
        ctx.db
            .acquire_online_lock(1, 880_011, Some(recruit_id), true, 1_700_000_000)
            .unwrap();
        let inv = invite_fleet_member(
            &ctx,
            InviteFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                inviter_id_override: None,
                invitee_id: recruit_id,
                role: FleetRole::SquadMember,
                wing_id: Some(wing_id),
                squad_id: Some(squad_id),
                enforce_online_and_capacity_checks: true,
                validate_hierarchy_strict: true,
                now_unix: 1_700_000_000,
            },
        )
        .unwrap();
        assert_eq!(inv.wing_id, wing_id);
        assert_eq!(inv.squad_id, squad_id);

        // Duplicate pending invite conflicts.
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    inviter_id_override: None,
                    invitee_id: recruit_id,
                    role: FleetRole::SquadMember,
                    wing_id: Some(wing_id),
                    squad_id: Some(squad_id),
                    enforce_online_and_capacity_checks: true,
                    validate_hierarchy_strict: true,
                    now_unix: 1_700_000_000,
                }
            )
            .unwrap_err()
            .code,
            "InviteAlreadyPending"
        );

        // 2. Active session change timer blocks accept_fleet_invite.
        let timer_now = ctx.now_unix();
        record_session_change_timer(&ctx, recruit_id, timer_now, 30_000_142, 10, "jump");
        let err_timer = accept_fleet_invite(
            &ctx,
            AcceptFleetInviteInput {
                char_id: recruit_id,
                fleet_hint: Some(fleet_id),
                now_unix: timer_now,
                current_solar_system_id: Some(30_000_142),
            },
        )
        .unwrap_err();
        assert_eq!(err_timer.code, "SessionChangeTimerActive");
        clear_session_change_timer(&ctx, recruit_id);

        let accepted = accept_fleet_invite(
            &ctx,
            AcceptFleetInviteInput {
                char_id: recruit_id,
                fleet_hint: Some(fleet_id),
                now_unix: timer_now + 20,
                current_solar_system_id: Some(30_000_142),
            },
        )
        .unwrap();
        assert_eq!(accepted.fleet_id, fleet_id);
        assert_eq!(accepted.role, FleetRole::SquadMember);
        clear_session_change_timer(&ctx, recruit_id);

        // 3. Create second wing/squad and test move_fleet_member under free_move=false vs true.
        let w2 = create_fleet_wing(&ctx, fleet_id, Some(boss_id), Some("Wing 2"), true, 7_800_000, false).unwrap();
        let s2 = create_fleet_squad(&ctx, fleet_id, Some(boss_id), w2.wing_id, Some("Squad 2"), true, 7_800_000, false).unwrap();

        // Self-move disallowed when free_move = false.
        let err_free = move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(recruit_id),
                target_id: recruit_id,
                new_role: FleetRole::SquadMember,
                wing_id: Some(w2.wing_id),
                squad_id: Some(s2.squad_id),
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: false,
                enforce_session_change_timer: false,
                now_unix: timer_now + 30,
            },
        )
        .unwrap_err();
        assert_eq!(err_free.code, "FleetPermissionDenied");

        // Boss promotes recruit to SquadCommander in Squad 2; valid move updates role, wing, squad, job.
        let mv = move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                target_id: recruit_id,
                new_role: FleetRole::SquadCommander,
                wing_id: Some(w2.wing_id),
                squad_id: Some(s2.squad_id),
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: false,
                enforce_session_change_timer: false,
                now_unix: timer_now + 40,
            },
        )
        .unwrap();
        assert_eq!(mv.after.role, FleetRole::SquadCommander);
        assert_eq!(mv.after.wing_id, Some(w2.wing_id));
        assert_eq!(mv.after.squad_id, Some(s2.squad_id));

        // Occupying FleetCommander fails PositionOccupied.
        let err_occ = move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                target_id: recruit_id,
                new_role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: false,
                enforce_session_change_timer: false,
                now_unix: timer_now + 50,
            },
        )
        .unwrap_err();
        assert_eq!(err_occ.code, "PositionOccupied");
    }

    #[test]
    fn remove_kick_and_disband_fleet_handle_boss_handover_disconnected_priority_and_cleanup() {
        let ctx = shared_test_ctx();
        let boss_id = 95_000_201i64;
        let disconnected_wc_id = 95_000_202i64;
        let online_sc_id = 95_000_203i64;
        let fleet_id = 7_700_020i64;
        let wing_id = 7_700_120i64;
        let squad_id = 7_700_220i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(boss_id, 20, 98_400_020, None, 0));
            seed_char(snap, make_char(disconnected_wc_id, 21, 98_400_020, None, 0));
            seed_char(snap, make_char(online_sc_id, 22, 98_400_020, None, 0));
        });
        seed_basic_fleet(&ctx, fleet_id, boss_id, wing_id, squad_id, false);
        ctx.db.with_mut_snapshot(|snap| {
            let f = snap.fleets.get_mut(&fleet_id).unwrap();
            f.members.push(disconnected_wc_id);
            f.members.push(online_sc_id);
            f.member_details.insert(
                disconnected_wc_id,
                FleetMemberRecord {
                    character_id: disconnected_wc_id,
                    role: FleetRole::WingCommander,
                    wing_id: Some(wing_id),
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Wing Commander".to_string(),
                    join_time_unix: 10,
                },
            );
            f.member_details.insert(
                online_sc_id,
                FleetMemberRecord {
                    character_id: online_sc_id,
                    role: FleetRole::SquadCommander,
                    wing_id: Some(wing_id),
                    squad_id: Some(squad_id),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Commander".to_string(),
                    join_time_unix: 20,
                },
            );
        });
        {
            let mut ex = ctx.fleet_extras.lock().unwrap();
            ex.disconnected_members
                .insert(disconnected_wc_id, 1_700_000_000);
            ex.adverts.insert(
                fleet_id,
                FleetAdvertState {
                    fleet_id,
                    leader_id: boss_id,
                    fleet_name: "Advert Fleet".to_string(),
                    description: String::new(),
                    solar_system_id: 30_000_142,
                    invite_scope: 3,
                    auto_join: false,
                    min_standing: -10.0,
                    min_security: -10.0,
                    allowed_entities: HashSet::new(),
                    _created_unix: 1_700_000_000,
                },
            );
        }

        // CannotKickBoss when forbid_boss_even_self = true.
        assert_eq!(
            kick_or_remove_fleet_member(&ctx, fleet_id, Some(online_sc_id), boss_id, true)
                .unwrap_err()
                .code,
            "CannotKickBoss"
        );
        assert_eq!(
            kick_or_remove_fleet_member(&ctx, fleet_id, Some(boss_id), boss_id, true)
                .unwrap_err()
                .code,
            "CannotKickBoss"
        );

        // Removing boss hands leadership & advert leader_id to connected member over disconnected higher-rank member.
        let out = remove_character_from_fleet_domain(&ctx, fleet_id, boss_id);
        assert!(out.was_member);
        assert!(!out.disbanded);
        let (_, new_boss_rec, _) = out.handover.expect("boss handover triggered");
        assert_eq!(new_boss_rec.character_id, online_sc_id);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.fleets[&fleet_id].boss_id),
            online_sc_id
        );
        assert_eq!(
            ctx.fleet_extras.lock().unwrap().adverts[&fleet_id].leader_id,
            online_sc_id
        );

        // Non-boss cannot disband.
        assert_eq!(
            disband_fleet(&ctx, fleet_id, disconnected_wc_id)
                .unwrap_err()
                .code,
            "FleetPermissionDenied"
        );

        // New boss disbands cleanly, clearing fleets and advert metadata.
        let disbanded = disband_fleet(&ctx, fleet_id, online_sc_id).unwrap();
        assert!(disbanded.former_member_ids.contains(&online_sc_id));
        assert!(disbanded.former_member_ids.contains(&disconnected_wc_id));
        assert!(ctx.db.with_snapshot(|s| !s.fleets.contains_key(&fleet_id)));
        assert!(!ctx.fleet_extras.lock().unwrap().adverts.contains_key(&fleet_id));
    }

    #[test]
    fn fleet_wing_and_squad_crud_handles_create_rename_delete_limits_and_cleanup() {
        let ctx = shared_test_ctx();
        let boss_id = 95_000_301i64;
        let member_id = 95_000_302i64;
        let fleet_id = 7_700_030i64;
        let wing_id = 7_700_130i64;
        let squad_id = 7_700_230i64;

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, make_char(boss_id, 30, 98_400_030, None, 0));
            seed_char(snap, make_char(member_id, 31, 98_400_030, None, 0));
        });
        seed_basic_fleet(&ctx, fleet_id, boss_id, wing_id, squad_id, true);
        ctx.db.with_mut_snapshot(|snap| {
            let f = snap.fleets.get_mut(&fleet_id).unwrap();
            f.members.push(member_id);
            f.member_details.insert(
                member_id,
                FleetMemberRecord {
                    character_id: member_id,
                    role: FleetRole::SquadMember,
                    wing_id: Some(wing_id),
                    squad_id: Some(squad_id),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Member".to_string(),
                    join_time_unix: 100,
                },
            );
        });

        // Non-FC cannot create wing or squad.
        assert_eq!(
            create_fleet_wing(&ctx, fleet_id, Some(member_id), Some("W2"), true, 7_800_100, false)
                .unwrap_err()
                .code,
            "FleetPermissionDenied"
        );
        assert_eq!(
            create_fleet_squad(
                &ctx,
                fleet_id,
                Some(member_id),
                wing_id,
                Some("S2"),
                true,
                7_800_100,
                false
            )
            .unwrap_err()
            .code,
            "FleetPermissionDenied"
        );

        // Cannot delete non-empty squad or wing when forbid_non_empty = true.
        assert_eq!(
            delete_fleet_squad(&ctx, fleet_id, Some(boss_id), squad_id, true)
                .unwrap_err()
                .code,
            "SquadNotEmpty"
        );
        assert_eq!(
            delete_fleet_wing(&ctx, fleet_id, Some(boss_id), wing_id, true)
                .unwrap_err()
                .code,
            "WingNotEmpty"
        );

        // Rename wing & squad.
        assert_eq!(
            rename_fleet_wing(&ctx, fleet_id, Some(boss_id), wing_id, "AlphaWing", true).unwrap(),
            "AlphaWing"
        );
        assert_eq!(
            rename_fleet_squad(&ctx, fleet_id, Some(boss_id), squad_id, "AlphaSq", true).unwrap(),
            "AlphaSq"
        );
        assert_eq!(
            rename_fleet_squad(
                &ctx,
                fleet_id,
                Some(boss_id),
                squad_id,
                "NameTooLong123",
                true
            )
            .unwrap_err()
            .code,
            "InvalidArgument"
        );

        // Max 5 wings per fleet & max 5 squads per wing.
        let mut created_wings = Vec::new();
        for i in 2..=5 {
            created_wings.push(
                create_fleet_wing(
                    &ctx,
                    fleet_id,
                    Some(boss_id),
                    Some(&format!("Wing {i}")),
                    true,
                    7_800_100,
                    false,
                )
                .unwrap(),
            );
        }
        assert_eq!(
            create_fleet_wing(&ctx, fleet_id, Some(boss_id), Some("Wing 6"), true, 7_800_100, false)
                .unwrap_err()
                .code,
            "MaxWingsReached"
        );

        let mut created_squads = Vec::new();
        for i in 2..=5 {
            created_squads.push(
                create_fleet_squad(
                    &ctx,
                    fleet_id,
                    Some(boss_id),
                    wing_id,
                    Some(&format!("Sq {i}")),
                    true,
                    7_800_100,
                    false,
                )
                .unwrap(),
            );
        }
        assert_eq!(
            create_fleet_squad(
                &ctx,
                fleet_id,
                Some(boss_id),
                wing_id,
                Some("Sq 6"),
                true,
                7_800_100,
                false
            )
            .unwrap_err()
            .code,
            "MaxSquadsReached"
        );

        // Delete an empty created squad & empty wing cleanly.
        let empty_sq = created_squads.pop().unwrap();
        assert!(delete_fleet_squad(&ctx, fleet_id, Some(boss_id), empty_sq.squad_id, true).is_ok());
        let empty_wg = created_wings.pop().unwrap();
        assert!(delete_fleet_wing(&ctx, fleet_id, Some(boss_id), empty_wg.wing_id, true).is_ok());
    }

    #[test]
    fn fleet_hierarchy_predicates_validate_edges_non_boss_fc_wc_sc_and_free_move() {
        // Cover non-boss FC, WC/SC missing wing/squad ids, self-touch vs peer FC, and validate_wing_and_squad fallbacks.
        let mut fleet = FleetRecord {
            fleet_id: 901,
            boss_id: 1,
            members: vec![1, 2, 3, 4, 5, 6, 7],
            wings: vec![10, 20],
            squads: vec![100, 200],
            ..Default::default()
        };
        fleet.wing_details.insert(
            10,
            FleetWingRecord {
                wing_id: 10,
                name: "W10".into(),
                commander_id: Some(3),
                squad_ids: vec![100],
            },
        );
        fleet.wing_details.insert(
            20,
            FleetWingRecord {
                wing_id: 20,
                name: "W20".into(),
                commander_id: None,
                squad_ids: vec![200],
            },
        );
        fleet.squad_details.insert(
            100,
            FleetSquadRecord {
                squad_id: 100,
                wing_id: 10,
                name: "S100".into(),
                commander_id: Some(4),
            },
        );
        for (cid, role, wid, sid) in [
            (2, FleetRole::FleetCommander, None, None),
            (3, FleetRole::WingCommander, Some(10), None),
            (4, FleetRole::SquadCommander, Some(10), Some(100)),
            (5, FleetRole::SquadMember, Some(10), Some(100)),
            (6, FleetRole::WingCommander, None, None),     // WC missing wing_id
            (7, FleetRole::SquadCommander, Some(10), None), // SC missing squad_id
        ] {
            fleet.member_details.insert(
                cid,
                FleetMemberRecord {
                    character_id: cid,
                    role,
                    wing_id: wid,
                    squad_id: sid,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 1,
                },
            );
        }

        // Boss without member_details -> (FleetCommander, None, None); bare member -> (SquadMember, first wing, first squad).
        assert_eq!(
            member_effective_position(&fleet, 1),
            (FleetRole::FleetCommander, None, None)
        );
        fleet.members.push(88);
        assert_eq!(
            member_effective_position(&fleet, 88),
            (FleetRole::SquadMember, Some(10), Some(100))
        );

        // Non-boss FC can manage any wing/squad, touch self, cannot touch boss or promote to FC.
        assert!(is_fleet_boss_or_fc(&fleet, 2));
        assert!(can_manage_wing(&fleet, 2, 20));
        assert!(can_manage_squad(&fleet, 2, 200));
        assert!(!can_manage_wing(&fleet, 999, 10));
        assert!(!can_manage_squad(&fleet, 999, 100));
        assert!(can_act_on_member(&fleet, 2, 2, None));
        assert!(!can_act_on_member(&fleet, 2, 1, None));
        assert!(!can_act_on_member(
            &fleet,
            2,
            5,
            Some((FleetRole::FleetCommander, None, None))
        ));

        // WC/SC edge branches in can_act_on_member and can_invite_member.
        assert!(!can_act_on_member(&fleet, 999, 5, None));
        assert!(!can_act_on_member(&fleet, 6, 5, None));
        assert!(!can_act_on_member(&fleet, 3, 2, None));
        assert!(!can_act_on_member(
            &fleet,
            3,
            4,
            Some((FleetRole::WingCommander, Some(10), None))
        ));
        assert!(!can_act_on_member(&fleet, 7, 5, None));
        assert!(!can_act_on_member(
            &fleet,
            4,
            5,
            Some((FleetRole::SquadCommander, Some(10), Some(100)))
        ));
        assert!(can_invite_member(&fleet, 2, FleetRole::WingCommander, Some(20), None));
        assert!(!can_invite_member(&fleet, 2, FleetRole::FleetCommander, None, None));
        assert!(!can_invite_member(&fleet, 6, FleetRole::SquadMember, None, Some(100)));
        assert!(!can_invite_member(&fleet, 3, FleetRole::WingCommander, Some(10), None));
        assert!(!can_invite_member(&fleet, 7, FleetRole::SquadMember, Some(10), Some(100)));
        assert!(!can_invite_member(&fleet, 4, FleetRole::SquadCommander, Some(10), Some(100)));
        assert!(!can_invite_member(&fleet, 999, FleetRole::SquadMember, Some(10), Some(100)));

        // validate_wing_and_squad error paths and non-move fallbacks.
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::SquadMember, Some(999), None, false)
                .unwrap_err()
                .code,
            "WingNotFound"
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::SquadMember, None, Some(999), false)
                .unwrap_err()
                .code,
            "SquadNotFound"
        );
        // Squad 200 is only listed in wing_details[20].squad_ids -> mismatch with wing 10 catches fallback check.
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::SquadMember, Some(10), Some(200), false)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::FleetCommander, Some(10), Some(100), true)
                .unwrap(),
            (None, None)
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::WingCommander, None, None, false)
                .unwrap(),
            (Some(10), None)
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::WingCommander, None, None, true)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::SquadMember, None, None, false)
                .unwrap(),
            (Some(10), Some(100))
        );
        assert_eq!(
            validate_wing_and_squad(&fleet, 901, FleetRole::SquadMember, None, None, true)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
    }

    #[test]
    fn fleet_lifecycle_accept_move_remove_wings_squads_cover_remaining_domain_branches() {
        let ctx = shared_test_ctx();
        let boss_id = 95_100_001i64;
        let fc_id = 95_100_002i64;
        let wc_id = 95_100_003i64;
        let sc_id = 95_100_004i64;
        let mem_id = 95_100_005i64;
        let bare_id = 95_100_006i64;
        let outsider_id = 95_100_099i64;
        let fleet_id = 7_710_001i64;
        let w1 = 7_710_101i64;
        let s1 = 7_710_201i64;

        ctx.db.with_mut_snapshot(|snap| {
            let base_acct = snap.accounts.values().next().cloned();
            for (idx, cid) in [boss_id, fc_id, wc_id, sc_id, mem_id, bare_id, outsider_id]
                .into_iter()
                .enumerate()
            {
                let uid = 100 + idx as u64;
                if !snap.accounts.values().any(|a| a.user_id == uid) {
                    if let Some(mut acct) = base_acct.clone() {
                        acct.user_id = uid;
                        acct.username = format!("fleet_test_{uid}");
                        snap.accounts.insert(acct.username.clone(), acct);
                    }
                }
                let mut ch = make_char(cid, uid, 98_410_001, None, 0);
                if cid == bare_id {
                    ch.fleet_id = Some(fleet_id);
                }
                seed_char(snap, ch);
            }
        });
        for (idx, cid) in [boss_id, fc_id, wc_id, sc_id, mem_id, bare_id, outsider_id]
            .into_iter()
            .enumerate()
        {
            ctx.db
                .acquire_online_lock(
                    100 + idx as u64,
                    7_700_000 + idx as u64,
                    Some(cid),
                    true,
                    1_700_000_000,
                )
                .unwrap();
        }
        seed_basic_fleet(&ctx, fleet_id, boss_id, w1, s1, true);
        let w2 = create_fleet_wing(&ctx, fleet_id, Some(boss_id), None, false, 10, true).unwrap();
        let s2 = create_fleet_squad(&ctx, fleet_id, Some(boss_id), w2.wing_id, None, false, 20, true)
            .unwrap();
        assert_eq!(w2.name, "Wing 2");
        assert_eq!(s2.name, "Squad 2");
        assert!(w2.wing_id >= 10);
        assert!(s2.squad_id >= 20);

        // Missing fleet / character checks across getters & mutators.
        assert_eq!(get_character_fleet(&ctx, 999_888_777).unwrap_err().code, "CharacterNotFound");
        assert_eq!(get_fleet_settings(&ctx, 999_999, None).unwrap_err().code, "FleetNotFound");
        assert_eq!(list_fleet_members(&ctx, 999_999, None).unwrap_err().code, "FleetNotFound");
        assert_eq!(list_fleet_wings(&ctx, 999_999, None).unwrap_err().code, "FleetNotFound");
        assert_eq!(disband_fleet(&ctx, 999_999, boss_id).unwrap_err().code, "FleetNotFound");
        assert_eq!(disband_fleet(&ctx, fleet_id, outsider_id).unwrap_err().code, "NotInFleet");
        assert_eq!(
            list_fleet_members(&ctx, fleet_id, Some(outsider_id)).unwrap_err().code,
            "NotInFleet"
        );
        assert_eq!(
            list_fleet_wings(&ctx, fleet_id, Some(outsider_id)).unwrap_err().code,
            "NotInFleet"
        );
        assert_eq!(
            kick_or_remove_fleet_member(&ctx, fleet_id, Some(outsider_id), boss_id, false)
                .unwrap_err()
                .code,
            "NotInFleet"
        );

        // Unchecked invite (enforce=false, strict=false) + already_in_fleet + auto_join fallback accept.
        assert_eq!(
            accept_fleet_invite(
                &ctx,
                AcceptFleetInviteInput {
                    char_id: mem_id,
                    fleet_hint: Some(fleet_id),
                    now_unix: 1_700_000_000,
                    current_solar_system_id: None,
                }
            )
            .unwrap_err()
            .code,
            "InviteExpired"
        );
        assert_eq!(
            accept_fleet_invite(
                &ctx,
                AcceptFleetInviteInput {
                    char_id: boss_id,
                    fleet_hint: Some(fleet_id),
                    now_unix: 1_700_000_000,
                    current_solar_system_id: None,
                }
            )
            .unwrap_err()
            .code,
            "AlreadyInFleet"
        );
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id: 999_999,
                    caller_char_id: None,
                    inviter_id_override: None,
                    invitee_id: mem_id,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    enforce_online_and_capacity_checks: false,
                    validate_hierarchy_strict: false,
                    now_unix: 1_700_000_000,
                }
            )
            .unwrap_err()
            .code,
            "FleetNotFound"
        );
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(outsider_id),
                    inviter_id_override: None,
                    invitee_id: mem_id,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    enforce_online_and_capacity_checks: false,
                    validate_hierarchy_strict: false,
                    now_unix: 1_700_000_000,
                }
            )
            .unwrap_err()
            .code,
            "NotInFleet"
        );
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: None,
                    inviter_id_override: Some(boss_id),
                    invitee_id: 999_888_777,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    enforce_online_and_capacity_checks: false,
                    validate_hierarchy_strict: false,
                    now_unix: 1_700_000_000,
                }
            )
            .unwrap_err()
            .code,
            "CharacterNotFound"
        );

        let unchecked_inv = invite_fleet_member(
            &ctx,
            InviteFleetMemberInput {
                fleet_id,
                caller_char_id: None,
                inviter_id_override: Some(555_555),
                invitee_id: wc_id,
                role: FleetRole::WingCommander,
                wing_id: Some(w2.wing_id),
                squad_id: None,
                enforce_online_and_capacity_checks: false,
                validate_hierarchy_strict: false,
                now_unix: 1_700_000_010,
            },
        )
        .unwrap();
        assert_eq!(unchecked_inv.inviter_id, 555_555);
        let acc_wc = accept_fleet_invite(
            &ctx,
            AcceptFleetInviteInput {
                char_id: wc_id,
                fleet_hint: Some(fleet_id),
                now_unix: 1_700_000_015,
                current_solar_system_id: None,
            },
        )
        .unwrap();
        assert_eq!(acc_wc.role, FleetRole::WingCommander);
        clear_session_change_timer(&ctx, wc_id);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.fleets[&fleet_id].wing_details[&w2.wing_id].commander_id),
            Some(wc_id)
        );

        // Accept with no invite_details uses auto_join_squads fallback.
        {
            ctx.fleet_extras
                .lock()
                .unwrap()
                .auto_join_squads
                .insert(fleet_id, Some(s2.squad_id));
            ctx.db.with_mut_snapshot(|s| {
                s.fleets.get_mut(&fleet_id).unwrap().invites.insert(mem_id);
            });
        }
        let acc_mem = accept_fleet_invite(
            &ctx,
            AcceptFleetInviteInput {
                char_id: mem_id,
                fleet_hint: None,
                now_unix: 1_700_000_020,
                current_solar_system_id: Some(30_000_142),
            },
        )
        .unwrap();
        assert_eq!(acc_mem.squad_id, s2.squad_id);
        clear_session_change_timer(&ctx, mem_id);

        // Occupied WC slot fails PositionOccupied when inviting SC/WC with strict check.
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    inviter_id_override: None,
                    invitee_id: sc_id,
                    role: FleetRole::WingCommander,
                    wing_id: Some(w2.wing_id),
                    squad_id: None,
                    enforce_online_and_capacity_checks: true,
                    validate_hierarchy_strict: true,
                    now_unix: 1_700_000_025,
                }
            )
            .unwrap_err()
            .code,
            "PositionOccupied"
        );
        // FleetFull check via max_sizes boundary (currently 3 members: boss, wc, mem).
        ctx.fleet_extras.lock().unwrap().max_sizes.insert(fleet_id, 3);
        assert_eq!(
            invite_fleet_member(
                &ctx,
                InviteFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    inviter_id_override: None,
                    invitee_id: sc_id,
                    role: FleetRole::SquadMember,
                    wing_id: Some(w1),
                    squad_id: Some(s1),
                    enforce_online_and_capacity_checks: true,
                    validate_hierarchy_strict: true,
                    now_unix: 1_700_000_026,
                }
            )
            .unwrap_err()
            .code,
            "FleetFull"
        );
        ctx.fleet_extras.lock().unwrap().max_sizes.insert(fleet_id, 50);

        // Seed sc_id & bare_id (bare_id in members without member_details) and move/commander coverage.
        ctx.db.with_mut_snapshot(|s| {
            let f = s.fleets.get_mut(&fleet_id).unwrap();
            f.members.push(fc_id);
            f.member_details.insert(
                fc_id,
                FleetMemberRecord {
                    character_id: fc_id,
                    role: FleetRole::FleetCommander,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(), // tests empty role_name fallback in list_fleet_members
                    join_time_unix: 10,
                },
            );
            f.members.push(sc_id);
            f.member_details.insert(
                sc_id,
                FleetMemberRecord {
                    character_id: sc_id,
                    role: FleetRole::SquadCommander,
                    wing_id: Some(w1),
                    squad_id: Some(s1),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Commander".into(),
                    join_time_unix: 11,
                },
            );
            f.squad_details.get_mut(&s1).unwrap().commander_id = Some(sc_id);
            f.members.push(bare_id);
        });

        // Non-boss FC can update settings when require_boss_only=false, rejected when true.
        let non_boss_upd = update_fleet_settings(
            &ctx,
            UpdateFleetSettingsInput {
                fleet_id,
                caller_char_id: Some(fc_id),
                require_boss_only: false,
                motd: None,
                is_free_move: None,
                is_registered: Some(true),
                is_voice_enabled: Some(true),
                is_loot_logging: Some(true),
            },
        )
        .unwrap();
        assert!(!non_boss_upd.old_registered && non_boss_upd.new_registered);
        assert!(get_fleet_settings(&ctx, fleet_id, None).unwrap().is_voice_enabled);
        assert_eq!(
            update_fleet_settings(
                &ctx,
                UpdateFleetSettingsInput {
                    fleet_id,
                    caller_char_id: Some(fc_id),
                    require_boss_only: true,
                    motd: Some("nope".into()),
                    is_free_move: None,
                    is_registered: None,
                    is_voice_enabled: None,
                    is_loot_logging: None,
                }
            )
            .unwrap_err()
            .code,
            "FleetPermissionDenied"
        );

        // Self free_move succeeds for SquadMember; moving to occupied WC/SC fails PositionOccupied.
        let free_mv = move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(mem_id),
                target_id: mem_id,
                new_role: FleetRole::SquadMember,
                wing_id: Some(w1),
                squad_id: Some(s1),
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: false,
                enforce_session_change_timer: true,
                now_unix: 1_700_000_100,
            },
        )
        .unwrap();
        assert!(free_mv.self_moved);
        // Active session timer recorded by free_mv blocks immediate second self-move.
        assert_eq!(
            move_fleet_member(
                &ctx,
                MoveFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(mem_id),
                    target_id: mem_id,
                    new_role: FleetRole::SquadMember,
                    wing_id: Some(w2.wing_id),
                    squad_id: Some(s2.squad_id),
                    strict_esi_wing_squad: true,
                    allow_fallback_missing_wing_squad: false,
                    enforce_session_change_timer: true,
                    now_unix: 1_700_000_101,
                }
            )
            .unwrap_err()
            .code,
            "SessionChangeTimerActive"
        );
        assert_eq!(
            move_fleet_member(
                &ctx,
                MoveFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    target_id: mem_id,
                    new_role: FleetRole::WingCommander,
                    wing_id: Some(w2.wing_id),
                    squad_id: None,
                    strict_esi_wing_squad: true,
                    allow_fallback_missing_wing_squad: false,
                    enforce_session_change_timer: false,
                    now_unix: 1_700_000_110,
                }
            )
            .unwrap_err()
            .code,
            "PositionOccupied"
        );
        assert_eq!(
            move_fleet_member(
                &ctx,
                MoveFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    target_id: mem_id,
                    new_role: FleetRole::SquadCommander,
                    wing_id: Some(w1),
                    squad_id: Some(s1),
                    strict_esi_wing_squad: true,
                    allow_fallback_missing_wing_squad: false,
                    enforce_session_change_timer: false,
                    now_unix: 1_700_000_111,
                }
            )
            .unwrap_err()
            .code,
            "PositionOccupied"
        );

        // Moving bare_id with allow_fallback_missing_wing_squad=false vs true & strict=false.
        assert_eq!(
            move_fleet_member(
                &ctx,
                MoveFleetMemberInput {
                    fleet_id,
                    caller_char_id: Some(boss_id),
                    target_id: bare_id,
                    new_role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    strict_esi_wing_squad: false,
                    allow_fallback_missing_wing_squad: false,
                    enforce_session_change_timer: false,
                    now_unix: 1_700_000_112,
                }
            )
            .unwrap_err()
            .code,
            "MemberNotFound"
        );
        let moved_bare = move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                target_id: bare_id,
                new_role: FleetRole::SquadCommander,
                wing_id: Some(w2.wing_id),
                squad_id: Some(s2.squad_id),
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: true,
                enforce_session_change_timer: false,
                now_unix: 1_700_000_113,
            },
        )
        .unwrap();
        assert_eq!(moved_bare.after.squad_id, Some(s2.squad_id));

        // Demoting wc_id clears old wing.commander_id.
        move_fleet_member(
            &ctx,
            MoveFleetMemberInput {
                fleet_id,
                caller_char_id: Some(boss_id),
                target_id: wc_id,
                new_role: FleetRole::SquadMember,
                wing_id: Some(w2.wing_id),
                squad_id: Some(s2.squad_id),
                strict_esi_wing_squad: true,
                allow_fallback_missing_wing_squad: false,
                enforce_session_change_timer: false,
                now_unix: 1_700_000_114,
            },
        )
        .unwrap();
        assert_eq!(
            ctx.db.with_snapshot(|s| s.fleets[&fleet_id].wing_details[&w2.wing_id].commander_id),
            None
        );

        // WC/SC permissions on rename/create + cascade wing delete with forbid_non_empty=false.
        rename_fleet_wing(&ctx, fleet_id, Some(boss_id), w1, "  TrimW1 ", true).unwrap();
        assert_eq!(
            ctx.db.with_snapshot(|s| s.fleets[&fleet_id].wing_details[&w1].name.clone()),
            "TrimW1"
        );
        assert_eq!(
            rename_fleet_wing(&ctx, fleet_id, Some(boss_id), w1, "   ", true)
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        assert!(rename_fleet_wing(&ctx, fleet_id, Some(sc_id), w1, "Blocked", true).is_err());
        rename_fleet_squad(&ctx, fleet_id, Some(sc_id), s1, "  OwnSq ", true).unwrap();
        assert_eq!(
            rename_fleet_squad(&ctx, fleet_id, Some(mem_id), s1, "Denied", true)
                .unwrap_err()
                .code,
            "FleetPermissionDenied"
        );

        // Non-empty delete with forbid_non_empty=false removes w2 and child s2 cleanly.
        delete_fleet_wing(&ctx, fleet_id, Some(boss_id), w2.wing_id, false).unwrap();
        ctx.db.with_snapshot(|s| {
            let f = &s.fleets[&fleet_id];
            assert!(!f.wings.contains(&w2.wing_id));
            assert!(!f.squads.contains(&s2.squad_id));
            assert!(!f.squad_details.contains_key(&s2.squad_id));
        });

        // Remove all remaining members one by one; final removal disbands the fleet and clears char.fleet_id.
        for cid in [mem_id, bare_id, sc_id, wc_id, fc_id] {
            let rm = remove_character_from_fleet_domain(&ctx, fleet_id, cid);
            assert!(rm.was_member);
            assert!(!rm.disbanded);
        }
        let last_rm = kick_or_remove_fleet_member(&ctx, fleet_id, Some(boss_id), boss_id, false)
            .unwrap();
        assert!(last_rm.disbanded);
        assert!(ctx.db.with_snapshot(|s| !s.fleets.contains_key(&fleet_id)));
    }
}
