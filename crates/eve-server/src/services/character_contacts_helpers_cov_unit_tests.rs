//! Unit tests covering `character_contacts_helpers` (`parse_id_list`,
//! `is_character_online`, `notify_watchlist_logon`, `notify_watchlist_presence`,
//! `handle_home_station_candidates`, `handle_self_destruct_clone`,
//! `check_mail_recipient_blocks`).

use super::*;
use crate::esi::runtime_state::EsiContactEntry;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn contacts_watchlist_online_status_and_mail_blocks_cover_all_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );

    assert_eq!(parse_id_list(None), Vec::<i64>::new());
    assert_eq!(parse_id_list(Some(&PyRep::Integer(42))), vec![42]);
    assert_eq!(
        parse_id_list(Some(&PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]))),
        vec![1, 2]
    );

    let char1 = 90_000_001i64;
    let char2 = 90_000_002i64;
    assert!(!is_character_online(&ctx, char1));

    ctx.db
        .acquire_online_lock_with_computer_hash(1, 98_701, Some(char1), None, true, 100)
        .expect("online lock");
    ctx.db
        .acquire_online_lock_with_computer_hash(2, 98_702, Some(char2), None, true, 100)
        .expect("watcher online lock");
    assert!(is_character_online(&ctx, char1));

    // Add watchlist + block entries in ESI contact state
    {
        let mut contacts = ctx.esi_state.char_contacts.write().unwrap();
        contacts
            .entry(char2)
            .or_default()
            .push(EsiContactEntry {
                contact_id: char1,
                contact_type: "character".into(),
                standing: 10.0,
                is_watched: true,
                is_blocked: false,
                label_ids: Vec::new(),
            });
    }
    notify_watchlist_logon(&ctx, char1);
    let notifs = ctx.drain_notifications_for_char(char2);
    assert!(notifs.iter().any(|n| n.event == "OnClientContactChange"));

    // Blocking sender rejects mail
    {
        let mut contacts = ctx.esi_state.char_contacts.write().unwrap();
        if let Some(entry) = contacts
            .entry(char2)
            .or_default()
            .iter_mut()
            .find(|e| e.contact_id == char1)
        {
            entry.is_blocked = true;
        }
    }
    let mut sess = SessionState::new(98_701);
    sess.user_id = Some(1);
    sess.character_id = Some(char1);
    assert_eq!(
        check_mail_recipient_blocks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(char2)])])
            .expect_err("blocked sender")
            .code,
        "ContactBlocked"
    );

    // home_station_candidates
    sess.station_id = Some(60_003_760);
    let cands = handle_home_station_candidates(&ctx, &sess).expect("home station candidates");
    assert!(matches!(cands, PyRep::List(l) if !l.is_empty()));

    // handle_self_destruct_clone: in space rejected, non-capsule rejected, capsule in station succeeds
    sess.station_id = None;
    assert!(handle_self_destruct_clone(&ctx, &mut sess).is_err());

    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(0);
    assert!(handle_self_destruct_clone(&ctx, &mut sess).is_err());

    let pod_id = 98_700_333i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: char1,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(pod_id);
    let home = handle_self_destruct_clone(&ctx, &mut sess).expect("capsule self destruct ok");
    assert!(matches!(home, PyRep::Integer(_)));
}
