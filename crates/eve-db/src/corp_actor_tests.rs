//! Targeted unit tests for DB-only [`CorpActor`] resolution and helpers.

use super::*;
use crate::corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE, ROLE_PERSONNEL_MANAGER, ROLE_TRADER};
use crate::{AllianceRecord, CorpGovState, CorpMemberRoleAssignment, InventoryFlag, InventoryItemRecord};
use eve_sde::NpcCorporationInfo;
use std::collections::HashSet;

fn test_char(char_id: i64, corp_id: i64, alliance_id: Option<i64>) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id, user_id: char_id as u64, name: format!("Pilot {char_id}"),
        corporation_id: corp_id, alliance_id, station_id: Some(60_003_760),
        solar_system_id: 30_000_142, home_station_id: 60_003_760, ..CharacterRecord::default()
    }
}

fn test_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.npc_corporations.insert(500_001, NpcCorporationInfo {
        corporation_id: 500_001, name: "Synthetic SDE NPC Corp".into(), ticker_name: "SNPC".into(),
        faction_id: Some(500_001), solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760), tax_rate: 0.11, ceo_id: None,
    });
    sde
}

#[test]
fn resolve_enforces_npc_exclusion_ceo_pending_stasis_and_location_scoping() {
    let sde = test_sde();
    let mut snap = WorldStateSnapshot::default();
    let (corp_id, ceo_id, member_id, excluded_id, npc_pilot_id) = (98_555_001, 91_501, 91_502, 91_503, 91_504);
    let (hq_id, base_id, other_id, now) = (60_003_760, 60_003_761, 60_003_762, 1_800_000_000);

    assert!(is_npc_corporation(&sde, 0) && is_npc_corporation(&sde, 1_000_044) && is_npc_corporation(&sde, 500_001));
    assert!(!is_npc_corporation(&sde, corp_id));

    snap.characters.insert(ceo_id, test_char(ceo_id, corp_id, Some(99_999_999)));
    snap.characters.insert(member_id, test_char(member_id, corp_id, None));
    snap.characters.insert(excluded_id, test_char(excluded_id, corp_id, None));
    snap.characters.insert(npc_pilot_id, test_char(npc_pilot_id, 1_000_044, Some(99_000_001)));

    let mut corp = CorporationRecord {
        corporation_id: corp_id,
        name: "Actor Corp".into(),
        ticker: "ACTR".into(),
        ceo_id,
        hq_station_id: Some(hq_id),
        members: HashSet::from([ceo_id, member_id]),
        ..CorporationRecord::default()
    };
    corp.member_roles.insert(member_id, ROLE_TRADER | ROLE_HANGAR_CAN_TAKE[0]);
    corp.member_roles.insert(excluded_id, ROLE_DIRECTOR);
    snap.corporations.insert(corp_id, corp);

    snap.items.insert(500_001, InventoryItemRecord {
        item_id: 500_001, type_id: 27, owner_id: corp_id, location_id: base_id,
        flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..InventoryItemRecord::default()
    });
    snap.corp_role_assignments.insert((corp_id, member_id), CorpMemberRoleAssignment {
        roles_at_hq: ROLE_HANGAR_CAN_TAKE[0], roles_at_base: ROLE_HANGAR_CAN_TAKE[1],
        roles_at_other: ROLE_HANGAR_CAN_TAKE[2], ..CorpMemberRoleAssignment::default()
    });
    let mut gov = CorpGovState::default();
    gov.member_bases.insert(member_id, base_id);
    gov.titles.insert(1, ("Ops".into(), ROLE_ACCOUNTANT | ROLE_PERSONNEL_MANAGER));
    gov.member_titles.insert(member_id, 1);
    gov.pending_role_grants.insert(member_id, (ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0], now + 86_400));
    gov.pending_title_role_grants.insert(member_id, (ROLE_PERSONNEL_MANAGER, now + 86_400));
    snap.corp_governance.insert(corp_id, gov);
    // 1. Unknown character fails closed; 2. NPC corp strips stale alliance_id; 3. Excluded member receives 0 roles; 4. CEO ignores ch.alliance_id
    let missing = CorpActor::resolve(&snap, &sde, 999_999, now);
    assert!(!missing.char_in_db && missing.require_member() == Err(CorpActorDenied::NoCorporation) && missing.role_tuple() == (false, 0));
    let npc = CorpActor::resolve(&snap, &sde, npc_pilot_id, now);
    assert!(npc.is_npc_corp && npc.is_member && npc.alliance_id.is_none() && npc.require_member() == Err(CorpActorDenied::NpcCorporation));
    let excluded = CorpActor::resolve(&snap, &sde, excluded_id, now);
    assert!(excluded.is_excluded && !excluded.is_member && !excluded.is_director() && excluded.effective_roles == 0 && excluded.require_role(ROLE_DIRECTOR) == Err(CorpActorDenied::NotMember));
    let ceo = CorpActor::resolve(&snap, &sde, ceo_id, now);
    assert!(ceo.is_ceo && ceo.is_director() && ceo.effective_roles == u64::MAX && ceo.alliance_id.is_none());
    // 5. Member pending stasis before 24h vs matured after 24h + container station resolution
    let pre = CorpActor::resolve_at(&snap, &sde, member_id, Some(hq_id), now + 100);
    assert!(!pre.is_director() && (pre.effective_roles & ROLE_TRADER) == ROLE_TRADER && (pre.effective_roles & ROLE_ACCOUNTANT) == ROLE_ACCOUNTANT);
    assert_eq!((pre.effective_roles & (ROLE_PERSONNEL_MANAGER | ROLE_HANGAR_CAN_TAKE[0]), pre.require_role(ROLE_DIRECTOR)), (0, Err(CorpActorDenied::RoleDenied)));
    let post_base = CorpActor::resolve_at(&snap, &sde, member_id, Some(500_001), now + 86_400);
    assert!(post_base.is_director() && (post_base.effective_roles & ROLE_HANGAR_CAN_TAKE[1]) != 0 && (post_base.effective_roles & ROLE_HANGAR_CAN_TAKE[2]) == 0 && post_base.require_role(ROLE_DIRECTOR).is_ok());
    assert_ne!(CorpActor::resolve_at(&snap, &sde, member_id, Some(other_id), now + 86_400).effective_roles & ROLE_HANGAR_CAN_TAKE[2], 0);
    assert_eq!(CorpActor::resolve(&snap, &sde, member_id, now + 86_400).effective_roles & (ROLE_HANGAR_CAN_TAKE[1] | ROLE_HANGAR_CAN_TAKE[2]), 0);
    snap.characters.get_mut(&member_id).unwrap().station_id = Some(base_id);
    let (db_ceo, db_global) = effective_corp_roles_at_db(&snap, corp_id, member_id, None, now + 86_400);
    assert!(!db_ceo && (db_global & ROLE_HANGAR_CAN_TAKE[1]) == 0);
    assert_ne!(effective_corp_roles_at_db(&snap, corp_id, member_id, Some(base_id), now + 86_400).1 & ROLE_HANGAR_CAN_TAKE[1], 0);
    // 6. Cross-corp, dissolved-corp, and excluded-member actors fail closed on alliance_id & membership
    let dissolved_corp = 98_555_999_i64;
    snap.alliances.insert(99_555_001, AllianceRecord { alliance_id: 99_555_001, name: "Actor Alliance".into(), short_name: "ACTA".into(), executor_corp_id: corp_id, member_corp_ids: HashSet::from([corp_id, dissolved_corp]) });
    assert_eq!((CorpActor::resolve(&snap, &sde, ceo_id, now).alliance_id, CorpActor::resolve(&snap, &sde, excluded_id, now).alliance_id), (Some(99_555_001), None));
    let wrong_corp = CorpActor::resolve_for_corp(&snap, &sde, corp_id, npc_pilot_id, None, now);
    assert!(!wrong_corp.is_member && wrong_corp.alliance_id.is_none() && wrong_corp.require_member() == Err(CorpActorDenied::NotMember));
    snap.characters.insert(91_505, test_char(91_505, dissolved_corp, Some(99_555_001)));
    assert_eq!(resolve_corp_alliance_id(&snap, &sde, dissolved_corp, snap.characters.get(&91_505)), None);
    let dissolved = CorpActor::resolve(&snap, &sde, 91_505, now);
    assert!(!dissolved.corp_exists && !dissolved.is_member && dissolved.alliance_id.is_none() && dissolved.require_member() == Err(CorpActorDenied::CorporationNotFound));

    // 7. CorpRoleBreakdown + promoted-only (`now_unix = None`) + effective_scoped_corp_roles_db parity
    let bd_pre = corp_role_breakdown(&snap, corp_id, member_id, now + 100);
    assert!(!bd_pre.is_ceo);
    assert_eq!(bd_pre.roles, ROLE_TRADER | ROLE_ACCOUNTANT);
    assert_eq!(bd_pre.roles_at_hq, ROLE_HANGAR_CAN_TAKE[0]);
    assert_eq!(bd_pre.roles_at_base, ROLE_HANGAR_CAN_TAKE[1]);
    assert_eq!(bd_pre.roles_at_other, ROLE_HANGAR_CAN_TAKE[2]);
    assert_eq!(
        effective_scoped_corp_roles_db(&snap, corp_id, member_id, now + 100),
        (bd_pre.roles_at_hq, bd_pre.roles_at_base, bd_pre.roles_at_other)
    );
    let bd_promoted_only = corp_role_breakdown_at(&snap, corp_id, member_id, None);
    assert_eq!(bd_promoted_only, bd_pre);
    assert_eq!(
        effective_corp_roles_promoted_only(&snap, corp_id, member_id, Some(base_id)),
        (false, ROLE_TRADER | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE[1])
    );
    let bd_matured_promoted_only = {
        snap.corp_governance
            .get_mut(&corp_id)
            .unwrap()
            .pending_role_grants
            .insert(member_id, (ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0], 0));
        corp_role_breakdown_at(&snap, corp_id, member_id, None)
    };
    assert_eq!(bd_matured_promoted_only.roles & ROLE_DIRECTOR, 0);
    assert_eq!(corp_role_breakdown(&snap, 98_999_999, member_id, now), CorpRoleBreakdown::default());
}

#[test]
fn pending_global_grant_preserves_scoped_roles_while_scoped_grant_masks_only_its_location() {
    let sde = test_sde();
    let mut snap = WorldStateSnapshot::default();
    let (corp_id, ceo_id, member_id) = (98_555_011, 91_601, 91_602);
    let (hq_id, base_id, other_id, now) = (60_003_760, 60_003_761, 60_003_762, 1_800_040_000);

    snap.characters.insert(ceo_id, test_char(ceo_id, corp_id, None));
    snap.characters.insert(member_id, test_char(member_id, corp_id, None));
    snap.corporations.insert(corp_id, CorporationRecord {
        corporation_id: corp_id,
        name: "Scoped Preservation Corp".into(),
        ticker: "SCPR".into(),
        ceo_id,
        hq_station_id: Some(hq_id),
        members: HashSet::from([ceo_id, member_id]),
        ..CorporationRecord::default()
    });
    let hq_roles = ROLE_DIRECTOR | ROLE_PERSONNEL_MANAGER | ROLE_HANGAR_CAN_TAKE[0];
    snap.corp_role_assignments.insert((corp_id, member_id), CorpMemberRoleAssignment {
        character_id: member_id,
        roles_at_hq: hq_roles,
        roles_at_base: ROLE_HANGAR_CAN_TAKE[1],
        roles_at_other: ROLE_HANGAR_CAN_TAKE[2],
        ..CorpMemberRoleAssignment::default()
    });
    let mut gov = CorpGovState::default();
    gov.member_bases.insert(member_id, base_id);
    gov.pending_role_grants.insert(member_id, (ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0], now + 3_600));
    snap.corp_governance.insert(corp_id, gov);

    // Unmatured global grant must NOT strip existing scoped roles from breakdown or effective_scoped_corp_roles_db.
    let bd = corp_role_breakdown(&snap, corp_id, member_id, now + 100);
    assert_eq!(bd.roles, 0);
    assert_eq!(bd.unmatured_mask, ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0]);
    assert_eq!((bd.roles_at_hq, bd.roles_at_base, bd.roles_at_other), (hq_roles, ROLE_HANGAR_CAN_TAKE[1], ROLE_HANGAR_CAN_TAKE[2]));
    assert_eq!(
        effective_scoped_corp_roles_db(&snap, corp_id, member_id, now + 100),
        (hq_roles, ROLE_HANGAR_CAN_TAKE[1], ROLE_HANGAR_CAN_TAKE[2])
    );

    // CorpActor::resolve_at still masks unmatured_role_bits from flattened location roles, while keeping non-pending HQ roles.
    let actor_global = CorpActor::resolve(&snap, &sde, member_id, now + 100);
    assert!(!actor_global.is_director() && actor_global.effective_roles == 0);
    let actor_hq = CorpActor::resolve_at(&snap, &sde, member_id, Some(hq_id), now + 100);
    assert_eq!(actor_hq.effective_roles, ROLE_PERSONNEL_MANAGER);

    // Pending scoped grant masks ONLY its matching location (HQ), leaving Base and Other untouched.
    snap.corp_governance
        .get_mut(&corp_id)
        .unwrap()
        .pending_scoped_role_grants
        .insert(member_id, (ROLE_HANGAR_CAN_TAKE[0] | ROLE_HANGAR_CAN_TAKE[3], 0, 0, now + 7_200));
    let bd_scoped = corp_role_breakdown(&snap, corp_id, member_id, now + 100);
    assert_eq!(bd_scoped.roles_at_hq, ROLE_DIRECTOR | ROLE_PERSONNEL_MANAGER);
    assert_eq!(bd_scoped.roles_at_base, ROLE_HANGAR_CAN_TAKE[1]);
    assert_eq!(bd_scoped.roles_at_other, ROLE_HANGAR_CAN_TAKE[2]);
    assert_eq!(
        effective_scoped_corp_roles_db(&snap, corp_id, member_id, now + 100),
        (ROLE_DIRECTOR | ROLE_PERSONNEL_MANAGER, ROLE_HANGAR_CAN_TAKE[1], ROLE_HANGAR_CAN_TAKE[2])
    );
    assert_eq!(CorpActor::resolve_at(&snap, &sde, member_id, Some(base_id), now + 100).effective_roles, ROLE_HANGAR_CAN_TAKE[1]);
    assert_eq!(CorpActor::resolve_at(&snap, &sde, member_id, Some(other_id), now + 100).effective_roles, ROLE_HANGAR_CAN_TAKE[2]);

    // Pending title role grant continues to suppress matching scoped title/role bits until maturity.
    snap.corp_governance
        .get_mut(&corp_id)
        .unwrap()
        .pending_title_role_grants
        .insert(member_id, (ROLE_PERSONNEL_MANAGER, now + 7_200));
    let bd_title = corp_role_breakdown(&snap, corp_id, member_id, now + 100);
    assert_eq!(bd_title.roles_at_hq, ROLE_DIRECTOR);

    // After maturity, global, scoped, and title pending grants apply cleanly.
    let bd_post = corp_role_breakdown(&snap, corp_id, member_id, now + 7_200);
    assert_eq!(bd_post.unmatured_mask, 0);
    assert_eq!(bd_post.roles, ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0] | ROLE_PERSONNEL_MANAGER);
    assert_eq!(bd_post.roles_at_hq, hq_roles | ROLE_HANGAR_CAN_TAKE[3]);
}
