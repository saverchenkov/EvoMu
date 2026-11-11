//! Edge-case unit tests for `quasar_corp_domain_auth`:
//! `live_caller_corp_in_snap`, `live_caller_corp`, `resolve_authorized_corp`,
//! `check_corp_admin_role`, and `require_corp_roles`.

use super::*;
use eve_db::{CorporationRecord, WorldRepository};
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};
use std::collections::HashSet;

const SERVICE_ROLE_ADMIN: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn auth_ctx(
    char_id: Option<u32>,
    corp_id: Option<u32>,
    service_role_mask: u64,
    corp_role_mask: u64,
) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session(
        Some(1),
        char_id,
        corp_id,
        corp_role_mask,
        "tranquility",
        vec![],
    );
    ctx.service_role_mask = service_role_mask;
    ctx
}

fn msg_with_corp(field: u32, corp_id: u32) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.set(1, DynamicValue::Uint32(corp_id));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(field, DynamicValue::Message(Box::new(inner)));
    msg
}

fn setup_db() -> WorldRepository {
    let db = WorldRepository::with_demo_seed();
    db.with_mut_snapshot(|snap| {
        // Player corp 98_000_001 with CEO 90_000_001, director 90_000_002, personnel 90_000_003, regular 90_000_004
        snap.corporations.insert(
            98_000_001,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_000_001; __corp.name = "Test Player Corp".into(); __corp.ticker = "TPC".into(); __corp.ceo_id = 90_000_001; __corp.members = HashSet::from([90_000_001, 90_000_002, 90_000_003, 90_000_004]); __corp.member_roles = [
                    (90_000_002, corp_roles::ROLE_DIRECTOR),
                    (90_000_003, corp_roles::ROLE_PERSONNEL_MANAGER),
                    (90_000_004, corp_roles::ROLE_TRADER),
                ]
                .into_iter()
                .collect(); __corp.ensure_wallet_divisions(); __corp },
        );
        let template_char = snap.characters.values().next().unwrap().clone();
        // Character 90_000_001 has NPC corp in snapshot (1_000_009) but is CEO of 98_000_001
        let mut c1 = template_char.clone();
        c1.character_id = 90_000_001;
        c1.corporation_id = 1_000_009;
        snap.characters.insert(90_000_001, c1);

        // Character 90_000_002 has player corp 98_000_001 directly (> 2_000_000 branch)
        for cid in [90_000_002, 90_000_003, 90_000_004] {
            let mut ch = template_char.clone();
            ch.character_id = cid;
            ch.corporation_id = 98_000_001;
            snap.characters.insert(cid, ch);
        }
        // Character 90_000_005 has zero corp_id in snapshot -> falls back to ctx_corp
        let mut c5 = template_char;
        c5.character_id = 90_000_005;
        c5.corporation_id = 0;
        snap.characters.insert(90_000_005, c5);
    });
    db
}

#[test]
fn live_caller_corp_covers_npc_override_player_and_fallbacks() {
    let db = setup_db();

    // No char in context -> None
    let no_char = auth_ctx(None, Some(98_000_001), 0, 0);
    assert_eq!(live_caller_corp(&no_char, &db), None);

    // Char in 1_000_000..2_000_000 with ctx_corp where char IS CEO/member -> returns ctx_corp
    let npc_with_valid_player_ctx = auth_ctx(Some(90_000_001), Some(98_000_001), 0, 0);
    assert_eq!(live_caller_corp(&npc_with_valid_player_ctx, &db), Some(98_000_001));

    // Char in 1_000_000..2_000_000 with ctx_corp where char is NOT member -> returns snapshot NPC corp
    let npc_with_unowned_ctx = auth_ctx(Some(90_000_001), Some(98_999_999), 0, 0);
    assert_eq!(live_caller_corp(&npc_with_unowned_ctx, &db), Some(1_000_009));

    // Char with corp_id >= 2_000_000 -> returns snapshot corp
    let player_ctx = auth_ctx(Some(90_000_002), None, 0, 0);
    assert_eq!(live_caller_corp(&player_ctx, &db), Some(98_000_001));

    // Char with corp_id == 0 -> falls back to ctx_corp
    let zero_corp_ctx = auth_ctx(Some(90_000_005), Some(98_000_777), 0, 0);
    assert_eq!(live_caller_corp(&zero_corp_ctx, &db), Some(98_000_777));

    // Unknown char -> falls back to ctx_corp
    let unknown_char_ctx = auth_ctx(Some(90_999_999), Some(98_000_888), 0, 0);
    assert_eq!(live_caller_corp(&unknown_char_ctx, &db), Some(98_000_888));
}

#[test]
fn resolve_authorized_corp_enforces_cross_corp_and_admin_overrides() {
    let db = setup_db();
    let empty = DynamicQuasarMessage::new("Req");

    // Unauthenticated (no char, no admin) -> forbidden
    let unauth = auth_ctx(None, None, 0, 0);
    assert_eq!(
        resolve_authorized_corp(&unauth, &empty, 2, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    // Same corp on explicit corp_field=2
    let member = auth_ctx(Some(90_000_002), Some(98_000_001), 0, 0);
    assert_eq!(
        resolve_authorized_corp(&member, &msg_with_corp(2, 98_000_001), 2, &db).unwrap(),
        98_000_001
    );

    // Fallback to field 1 when corp_field != 1 and field 2 is absent
    assert_eq!(
        resolve_authorized_corp(&member, &msg_with_corp(1, 98_000_001), 2, &db).unwrap(),
        98_000_001
    );

    // None requested -> falls back to caller's own corp
    assert_eq!(
        resolve_authorized_corp(&member, &empty, 1, &db).unwrap(),
        98_000_001
    );

    // Cross-corp denied for non-admin
    let err = resolve_authorized_corp(&member, &msg_with_corp(1, 98_000_099), 1, &db).unwrap_err();
    assert_eq!(err.0, STATUS_FORBIDDEN);
    assert!(err.1.contains("Cross-corporation"));

    // Cross-corp allowed for admin
    let admin_member = auth_ctx(Some(90_000_002), Some(98_000_001), SERVICE_ROLE_ADMIN, 0);
    assert_eq!(
        resolve_authorized_corp(&admin_member, &msg_with_corp(1, 98_000_099), 1, &db).unwrap(),
        98_000_099
    );

    // Admin with no character and no ctx corp can target requested corp
    let pure_admin = auth_ctx(None, None, SERVICE_ROLE_ADMIN, 0);
    assert_eq!(
        resolve_authorized_corp(&pure_admin, &msg_with_corp(1, 98_000_099), 1, &db).unwrap(),
        98_000_099
    );

    // Admin with no requested corp and no ctx corp -> Corporation context required
    assert_eq!(
        resolve_authorized_corp(&pure_admin, &empty, 1, &db).unwrap_err().0,
        STATUS_FORBIDDEN
    );
}

#[test]
fn check_corp_admin_role_and_require_corp_roles_matrix() {
    let db = setup_db();

    // No char
    assert_eq!(
        check_corp_admin_role(&auth_ctx(None, Some(98_000_001), 0, 0), &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Missing corp context
    assert_eq!(
        check_corp_admin_role(&auth_ctx(Some(90_000_005), None, 0, 0), &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // CEO, Director, Personnel Manager, and Service Admin all pass check_corp_admin_role
    assert_eq!(
        check_corp_admin_role(&auth_ctx(Some(90_000_001), Some(98_000_001), 0, 0), &db).unwrap(),
        (90_000_001, 98_000_001)
    );
    assert_eq!(
        check_corp_admin_role(&auth_ctx(Some(90_000_002), None, 0, 0), &db).unwrap(),
        (90_000_002, 98_000_001)
    );
    assert_eq!(
        check_corp_admin_role(&auth_ctx(Some(90_000_003), None, 0, 0), &db).unwrap(),
        (90_000_003, 98_000_001)
    );
    assert_eq!(
        check_corp_admin_role(
            &auth_ctx(Some(90_000_004), None, SERVICE_ROLE_ADMIN, 0),
            &db
        )
        .unwrap(),
        (90_000_004, 98_000_001)
    );

    // Regular member without admin/director/personnel fails check_corp_admin_role
    assert_eq!(
        check_corp_admin_role(&auth_ctx(Some(90_000_004), None, 0, 0), &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // require_corp_roles checks
    assert!(
        require_corp_roles(&auth_ctx(None, None, SERVICE_ROLE_ADMIN, 0), &db, 98_000_001, 0)
            .is_ok()
    );
    assert_eq!(
        require_corp_roles(&auth_ctx(None, None, 0, 0), &db, 98_000_001, corp_roles::ROLE_TRADER)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Wrong corp
    assert_eq!(
        require_corp_roles(
            &auth_ctx(Some(90_000_002), None, 0, 0),
            &db,
            98_000_999,
            corp_roles::ROLE_TRADER
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    // CEO / Director / matching Trader pass
    assert!(require_corp_roles(
        &auth_ctx(Some(90_000_001), Some(98_000_001), 0, 0),
        &db,
        98_000_001,
        corp_roles::ROLE_ACCOUNTANT
    )
    .is_ok());
    assert!(require_corp_roles(
        &auth_ctx(Some(90_000_002), None, 0, 0),
        &db,
        98_000_001,
        corp_roles::ROLE_ACCOUNTANT
    )
    .is_ok());
    assert!(require_corp_roles(
        &auth_ctx(Some(90_000_004), None, 0, 0),
        &db,
        98_000_001,
        corp_roles::ROLE_TRADER
    )
    .is_ok());
    // Missing required role (Trader asking for Accountant)
    assert_eq!(
        require_corp_roles(
            &auth_ctx(Some(90_000_004), None, 0, 0),
            &db,
            98_000_001,
            corp_roles::ROLE_ACCOUNTANT
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
}
