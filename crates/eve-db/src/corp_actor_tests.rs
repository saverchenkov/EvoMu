//! Targeted unit tests for DB-only [`CorpActor`] resolution and helpers.

use super::*;
use crate::corp_roles::{
    ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE, ROLE_PERSONNEL_MANAGER, ROLE_TRADER,
};
use crate::{AllianceRecord, CorpGovState, CorpMemberRoleAssignment, InventoryFlag, InventoryItemRecord};
use eve_sde::NpcCorporationInfo;
use std::collections::HashSet;

fn test_char(char_id: i64, corp_id: i64, alliance_id: Option<i64>) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id,
        user_id: char_id as u64,
        name: format!("Pilot {char_id}"),
        corporation_id: corp_id,
        alliance_id,
        station_id: Some(60_003_760),
        solar_system_id: 30_000_142,
        home_station_id: 60_003_760,
        ..CharacterRecord::default()
    }
}

fn test_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.npc_corporations.insert(
        500_001,
        NpcCorporationInfo {
            corporation_id: 500_001,
            name: "Synthetic SDE NPC Corp".into(),
            ticker_name: "SNPC".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(30_000_142),
            station_id: Some(60_003_760),
            tax_rate: 0.11,
            ceo_id: None,
        },
    );
    sde
}

#[test]
fn resolve_enforces_npc_exclusion_ceo_pending_stasis_and_location_scoping() {
    let sde = test_sde();
    let mut snap = WorldStateSnapshot::default();
    let corp_id = 98_555_001;
    let ceo_id = 91_501;
    let member_id = 91_502;
    let excluded_id = 91_503;
    let npc_pilot_id = 91_504;
    let hq_id = 60_003_760;
    let base_id = 60_003_761;
    let other_id = 60_003_762;
    let now = 1_800_000_000;

    assert!(is_npc_corporation(&sde, 0));
    assert!(is_npc_corporation(&sde, 1_000_044));
    assert!(is_npc_corporation(&sde, 500_001));
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
        alliance_id: None,
        hq_station_id: Some(hq_id),
        members: HashSet::from([ceo_id, member_id]),
        ..CorporationRecord::default()
    };
    corp.member_roles
        .insert(member_id, ROLE_TRADER | ROLE_HANGAR_CAN_TAKE[0]);
    corp.member_roles.insert(excluded_id, ROLE_DIRECTOR);
    snap.corporations.insert(corp_id, corp);

    snap.items.insert(
        500_001,
        InventoryItemRecord {
            item_id: 500_001,
            type_id: 27,
            owner_id: corp_id,
            location_id: base_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..InventoryItemRecord::default()
        },
    );
    snap.corp_role_assignments.insert(
        (corp_id, member_id),
        CorpMemberRoleAssignment {
            roles_at_hq: ROLE_HANGAR_CAN_TAKE[0],
            roles_at_base: ROLE_HANGAR_CAN_TAKE[1],
            roles_at_other: ROLE_HANGAR_CAN_TAKE[2],
            ..CorpMemberRoleAssignment::default()
        },
    );
    let mut gov = CorpGovState::default();
    gov.member_bases.insert(member_id, base_id);
    gov.titles
        .insert(1, ("Ops".into(), ROLE_ACCOUNTANT | ROLE_PERSONNEL_MANAGER));
    gov.member_titles.insert(member_id, 1);
    gov.pending_role_grants
        .insert(member_id, (ROLE_DIRECTOR | ROLE_HANGAR_CAN_TAKE[0], now + 86_400));
    gov.pending_title_role_grants
        .insert(member_id, (ROLE_PERSONNEL_MANAGER, now + 86_400));
    snap.corp_governance.insert(corp_id, gov);

    // 1. Unknown character fails closed
    let missing = CorpActor::resolve(&snap, &sde, 999_999, now);
    assert!(!missing.char_in_db);
    assert_eq!(missing.require_member(), Err(CorpActorDenied::NoCorporation));
    assert_eq!(missing.role_tuple(), (false, 0));

    // 2. NPC corp character strips stale alliance_id and denies governance
    let npc = CorpActor::resolve(&snap, &sde, npc_pilot_id, now);
    assert!(npc.is_npc_corp);
    assert!(npc.is_member);
    assert_eq!(npc.alliance_id, None);
    assert_eq!(npc.require_member(), Err(CorpActorDenied::NpcCorporation));

    // 3. Excluded member receives zero roles even if member_roles has ROLE_DIRECTOR
    let excluded = CorpActor::resolve(&snap, &sde, excluded_id, now);
    assert!(excluded.is_excluded);
    assert!(!excluded.is_member);
    assert!(!excluded.is_director());
    assert_eq!(excluded.effective_roles, 0);
    assert_eq!(excluded.require_role(ROLE_DIRECTOR), Err(CorpActorDenied::NotMember));

    // 4. CEO ignores stale ch.alliance_id when corp record exists with alliance_id == None
    let ceo = CorpActor::resolve(&snap, &sde, ceo_id, now);
    assert!(ceo.is_ceo);
    assert!(ceo.is_director());
    assert_eq!(ceo.effective_roles, u64::MAX);
    assert_eq!(ceo.alliance_id, None);

    // 5. Member pending stasis before 24h vs matured after 24h + container station resolution
    let pre = CorpActor::resolve_at(&snap, &sde, member_id, Some(hq_id), now + 100);
    assert!(!pre.is_director());
    assert_eq!(pre.effective_roles & ROLE_TRADER, ROLE_TRADER);
    assert_eq!(pre.effective_roles & ROLE_ACCOUNTANT, ROLE_ACCOUNTANT);
    assert_eq!(pre.effective_roles & ROLE_PERSONNEL_MANAGER, 0);
    assert_eq!(pre.effective_roles & ROLE_HANGAR_CAN_TAKE[0], 0);
    assert_eq!(pre.require_role(ROLE_DIRECTOR), Err(CorpActorDenied::RoleDenied));

    let post_base = CorpActor::resolve_at(&snap, &sde, member_id, Some(500_001), now + 86_400);
    assert!(post_base.is_director());
    assert_ne!(post_base.effective_roles & ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(post_base.effective_roles & ROLE_HANGAR_CAN_TAKE[2], 0);
    assert_eq!(post_base.require_role(ROLE_DIRECTOR), Ok(()));

    let post_other = CorpActor::resolve_at(&snap, &sde, member_id, Some(other_id), now + 86_400);
    assert_ne!(post_other.effective_roles & ROLE_HANGAR_CAN_TAKE[2], 0);

    let global_post = CorpActor::resolve(&snap, &sde, member_id, now + 86_400);
    assert_eq!(global_post.effective_roles & ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(global_post.effective_roles & ROLE_HANGAR_CAN_TAKE[2], 0);
    snap.characters.get_mut(&member_id).unwrap().station_id = Some(base_id);
    let (db_ceo, db_global) =
        effective_corp_roles_at_db(&snap, corp_id, member_id, None, now + 86_400);
    assert!(!db_ceo);
    assert_eq!(db_global & ROLE_HANGAR_CAN_TAKE[1], 0);
    let (_, db_base) =
        effective_corp_roles_at_db(&snap, corp_id, member_id, Some(base_id), now + 86_400);
    assert_ne!(db_base & ROLE_HANGAR_CAN_TAKE[1], 0);

    // 6. Cross-corp resolve_for_corp denies even CEO if character's DB corp changed, and AllianceRecord fallback resolves
    snap.alliances.insert(
        99_555_001,
        AllianceRecord {
            alliance_id: 99_555_001,
            name: "Actor Alliance".into(),
            short_name: "ACTA".into(),
            executor_corp_id: corp_id,
            member_corp_ids: HashSet::from([corp_id]),
        },
    );
    let with_ally = CorpActor::resolve(&snap, &sde, ceo_id, now);
    assert_eq!(with_ally.alliance_id, Some(99_555_001));

    let wrong_corp = CorpActor::resolve_for_corp(&snap, &sde, corp_id, npc_pilot_id, None, now);
    assert!(!wrong_corp.is_member);
    assert_eq!(wrong_corp.require_member(), Err(CorpActorDenied::NotMember));
}
