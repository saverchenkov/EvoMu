//! Unit tests for `#931` Track 3A compare-only telemetry & cross-scope pairing.

use super::*;
use crate::router::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications, NotificationTarget,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, RowChange, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use std::sync::Arc;

struct AlwaysOnlineLookup;
impl OnlineCharacterLookup for AlwaysOnlineLookup {
    fn is_character_online(&self, _char_id: i64, _user_id: u64) -> bool {
        true
    }
}

fn item_with(
    template: &InventoryItemRecord,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    let mut rec = template.clone();
    rec.item_id = item_id;
    rec.type_id = type_id;
    rec.owner_id = owner_id;
    rec.location_id = location_id;
    rec.flag = flag;
    rec.quantity = quantity;
    rec.singleton = singleton;
    rec
}

fn fields_for(
    item_id: i64,
    type_id: Option<i64>,
    owner_id: i64,
    location_id: i64,
    flag_id: i64,
    wire_qty: i64,
) -> ItemChangeFields {
    ItemChangeFields {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag_id,
        wire_qty,
        change_dict: None,
    }
}

#[test]
fn breakdown_bucket_cap_and_rpc_label_normalization() {
    assert_eq!(normalize_dispatch_method_label("invbroker", "Add"), "invbroker.Add");
    assert_eq!(normalize_dispatch_method_label("", "Add"), "Add");
    assert_eq!(normalize_dispatch_method_label("bad.svc", "Add"), "<unknown>");
    assert_eq!(normalize_dispatch_method_label("invbroker", "bad meth"), "<unknown>");
    assert_eq!(normalize_dispatch_method_label("invbroker", "a\nb"), "<unknown>");
    assert_eq!(normalize_dispatch_method_label(&"x".repeat(65), "Add"), "<unknown>");
    assert_eq!(normalize_dispatch_method_label("", crate::flight_recorder::UNKNOWN_RPC_LABEL), "<unknown>");
    assert_eq!(normalize_dispatch_method_label("", OTHER_RPC_BUCKET), "<unknown>");
    assert_eq!(normalize_dispatch_method_label("invbroker", OTHER_RPC_BUCKET), "<unknown>");
    assert_eq!(normalize_dispatch_method_label(OTHER_CLASS_BUCKET, "Add"), "<unknown>");

    let cid = 90_000_001_i64;
    let mut ledger = ItemCompareLedger::default();
    // Fill MAX_COMPARE_BREAKDOWN_BUCKETS (512) retired projected_only buckets first so subsequent
    // retirements hit `<other_class>` / `<other_rpc>`, then clear pending slots in Dispatch.
    for i in 0..MAX_COMPARE_BREAKDOWN_BUCKETS {
        let item_id = 700_000 + i as i64;
        ledger.record_projected_batch(
            CompareOriginScope::Dispatch,
            Some(format!("svc.Meth{i}")),
            vec![(cid, "itemID", fields_for(item_id, Some(34), cid, 60_003_760, 4, 1), format!("class_{i}"))],
            Vec::new(),
            0,
        );
        ledger.record_projected_batch(
            CompareOriginScope::Dispatch,
            Some("svc.Meth0".to_string()),
            vec![(cid, "itemID", fields_for(item_id, Some(34), cid, 60_003_760, 4, 2), "class_0".to_string())],
            Vec::new(),
            0,
        );
        ledger.match_or_record_item_row(
            CompareOriginScope::Dispatch,
            "notify_clear_bucket_filler",
            &[(cid, None)],
            "itemID",
            Some(&fields_for(item_id, Some(34), cid, 60_003_760, 4, 2)),
        );
    }
    assert_eq!(ledger.summary().item_change.projected_only, MAX_COMPARE_BREAKDOWN_BUCKETS as u64);
    assert_eq!(ledger.summary().item_change.projected_only_by_class.len(), MAX_COMPARE_BREAKDOWN_BUCKETS);
    assert_eq!(ledger.summary().item_change.projected_only_by_dispatch_method.len(), MAX_COMPARE_BREAKDOWN_BUCKETS);

    // Push 4 overflow items; displace overflow_id into unmatched_projected_items (`<other_*>` = 1).
    for i in MAX_COMPARE_BREAKDOWN_BUCKETS..(MAX_COMPARE_BREAKDOWN_BUCKETS + 4) {
        let item_id = 700_000 + i as i64;
        ledger.record_projected_batch(
            CompareOriginScope::Dispatch,
            Some(format!("svc.Meth{i}")),
            vec![(cid, "itemID", fields_for(item_id, Some(34), cid, 60_003_760, 4, 1), format!("class_{i}"))],
            Vec::new(),
            0,
        );
    }
    let overflow_id = 700_000 + (MAX_COMPARE_BREAKDOWN_BUCKETS as i64) + 1;
    ledger.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("svc.Meth0".to_string()),
        vec![(cid, "itemID", fields_for(overflow_id, Some(34), cid, 60_003_760, 4, 99), "class_0".to_string())],
        Vec::new(),
        0,
    );
    ledger.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "notify_clear_displacer",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(overflow_id, Some(34), cid, 60_003_760, 4, 99)),
    );
    let mid = ledger.summary();
    assert_eq!(mid.item_change.projected_only, (MAX_COMPARE_BREAKDOWN_BUCKETS + 1) as u64);
    assert_eq!(mid.item_change.projected_only_by_class.get(OTHER_CLASS_BUCKET), Some(&1));
    assert_eq!(mid.item_change.projected_only_by_dispatch_method.get(OTHER_RPC_BUCKET), Some(&1));

    // Cross-scope match against retired overflow_id must unwind `<other_class>` and `<other_rpc>`.
    ledger.match_or_record_item_row(
        CompareOriginScope::Tick,
        "notify_item_record_change",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(overflow_id, Some(34), cid, 60_003_760, 4, 1)),
    );
    let unwound = ledger.summary();
    assert_eq!(unwound.item_change.matched, (MAX_COMPARE_BREAKDOWN_BUCKETS + 1) as u64);
    assert_eq!(unwound.item_change.matched_across_scopes, 1);
    assert_eq!(unwound.item_change.projected_only, MAX_COMPARE_BREAKDOWN_BUCKETS as u64);
    assert!(!unwound.item_change.projected_only_by_class.contains_key(OTHER_CLASS_BUCKET));
    assert!(!unwound.item_change.projected_only_by_dispatch_method.contains_key(OTHER_RPC_BUCKET));

    let summary = ledger.flush_pending();
    assert_eq!(summary.item_change.matched, (MAX_COMPARE_BREAKDOWN_BUCKETS + 1) as u64);
    assert_eq!(summary.item_change.matched_across_scopes, 1);
    assert_eq!(
        summary.item_change.projected_only,
        (MAX_COMPARE_BREAKDOWN_BUCKETS + 3) as u64
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get(OTHER_CLASS_BUCKET),
        Some(&3)
    );
    assert_eq!(
        summary
            .item_change
            .projected_only_by_dispatch_method
            .get(OTHER_RPC_BUCKET),
        Some(&3)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.values().sum::<u64>(),
        summary.item_change.projected_only
    );
}

#[test]
fn projected_only_class_counts_are_exact_beyond_sample_cap_across_all_kinds_and_scopes() {
    let mut sde = SdeRegistry::default();
    let citadel_type_id = 35_832_u32;
    sde.types.insert(
        citadel_type_id,
        eve_sde::InvType {
            type_id: citadel_type_id,
            group_id: 1657,
            category_id: 65,
            name: "Astrahus".into(),
            mass: 0.0,
            volume: 800_000.0,
            capacity: 0.0,
            radius: 5_000.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    let db = WorldRepository::with_demo_seed();
    let mut snap = db.with_snapshot(|s| s.clone());
    let template = snap.items.values().next().expect("demo seed item").clone();

    let char_id = *snap.characters.keys().next().expect("demo seed character");
    let ship_id = 88_000_001_i64;
    let structure_id = 88_000_002_i64;
    let sde_struct_item_id = 88_000_003_i64;
    let station_id = 60_003_760_i64;
    let system_id = 30_000_142_i64;
    let player_corp_id = 98_000_123_i64;
    let npc_corp_id = 1_000_125_i64;

    snap.items.insert(
        ship_id,
        item_with(&template, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
    );
    snap.items.insert(
        sde_struct_item_id,
        item_with(&template, sde_struct_item_id, citadel_type_id, player_corp_id, system_id, InventoryFlag::None, 1, true),
    );
    snap.structures.insert(
        structure_id,
        UpwellStructureRecord {
            structure_id,
            type_id: citadel_type_id,
            owner_corp_id: player_corp_id,
            solar_system_id: system_id as u32,
            name: "Test Astrahus".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 1.0,
            armor_hp: 1.0,
            hull_hp: 1.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        },
    );

    let bridge = ItemCompareBridge::new(1);
    {
        let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.enabled_override = Some(true);
        ledger.report_path_override = Some(None);
    }
    let online = AlwaysOnlineLookup;
    let empty_effects = TxItemEffectKeys::default();

    // 20 (> MAX_COMPARE_SAMPLES=16) dispatch inserts: character singleton in station hangar (flag 4).
    let mut dispatch_changes = ChangeSet::default();
    for idx in 0..20_i64 {
        let iid = 91_000_000 + idx;
        let rec = item_with(&template, iid, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        snap.items.insert(iid, rec.clone());
        dispatch_changes.items.push(RowChange { key: iid, before: None, after: Some(Arc::new(rec)) });
    }
    // 3 stacks moved from station -> ship cargo (flag 5) in dispatch.
    for idx in 0..3_i64 {
        let iid = 91_000_100 + idx;
        let before = item_with(&template, iid, 34, char_id, station_id, InventoryFlag::Hangar, 50, false);
        let after = item_with(&template, iid, 34, char_id, ship_id, InventoryFlag::Cargo, 50, false);
        snap.items.insert(iid, after.clone());
        dispatch_changes.items.push(RowChange { key: iid, before: Some(Arc::new(before)), after: Some(Arc::new(after)) });
    }
    // 2 items recycled from ship cargo -> location 6 (quantity 0) in dispatch.
    for idx in 0..2_i64 {
        let iid = 91_000_200 + idx;
        let before = item_with(&template, iid, 34, char_id, ship_id, InventoryFlag::Cargo, 10, false);
        dispatch_changes.items.push(RowChange { key: iid, before: Some(Arc::new(before)), after: None });
    }
    bridge.on_commit(
        &snap,
        &dispatch_changes,
        &sde,
        1_700_000_000,
        &online,
        &empty_effects,
        Some((1, Some("invbroker.Move".to_string()))),
    );

    // Tick scope: 2 singletons owned by character in solar system (flag 0).
    let mut tick_changes = ChangeSet::default();
    for idx in 0..2_i64 {
        let iid = 91_000_300 + idx;
        let rec = item_with(&template, iid, 587, char_id, system_id, InventoryFlag::None, 1, true);
        snap.items.insert(iid, rec.clone());
        tick_changes.items.push(RowChange { key: iid, before: None, after: Some(Arc::new(rec)) });
    }
    {
        let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
        *ledger.tick_sync_depths_by_thread.entry(std::thread::current().id()).or_insert(0) += 1;
    }
    bridge.on_commit(&snap, &tick_changes, &sde, 1_700_000_001, &online, &empty_effects, None);
    {
        let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.tick_sync_depths_by_thread.remove(&std::thread::current().id());
    }

    // Classifier assertions for NPC corp solar-system, player-corp structure (both structures table
    // and SDE category 65 item lookup), location_id == 0 ("none"), and unknown location ("other").
    let empty_map: HashMap<i64, ItemBeforeAfter<'_>> = HashMap::new();
    assert_eq!(
        classify_projected_item(
            None,
            &fields_for(91_000_400, Some(587), npc_corp_id, system_id, 0, -1),
            &snap,
            &ChangeSet::default(),
            &empty_map,
            &sde,
        ),
        "before=none,after=solar_system,flag=0,owner=npc_corp_or_other,singleton=true"
    );
    assert_eq!(
        classify_projected_item(
            None,
            &fields_for(91_000_401, Some(34), player_corp_id, structure_id, 4, 25),
            &snap,
            &ChangeSet::default(),
            &empty_map,
            &sde,
        ),
        "before=none,after=structure,flag=4,owner=player_corp,singleton=false"
    );
    assert_eq!(
        classify_projected_item(
            None,
            &fields_for(91_000_402, Some(34), player_corp_id, sde_struct_item_id, 4, 15),
            &snap,
            &ChangeSet::default(),
            &empty_map,
            &sde,
        ),
        "before=none,after=structure,flag=4,owner=player_corp,singleton=false"
    );
    assert_eq!(
        classify_projected_item(
            None,
            &fields_for(91_000_403, Some(34), char_id, 0, 0, 10),
            &snap,
            &ChangeSet::default(),
            &empty_map,
            &sde,
        ),
        "before=none,after=none,flag=0,owner=character,singleton=false"
    );
    assert_eq!(
        classify_projected_item(
            None,
            &fields_for(91_000_404, Some(34), char_id, 999, 0, 10),
            &snap,
            &ChangeSet::default(),
            &empty_map,
            &sde,
        ),
        "before=none,after=other,flag=0,owner=character,singleton=false"
    );

    // Neither scope: 2 stacks in structure owned by character (flag 4) + 1 stack at unknown "other" location.
    let mut neither_changes = ChangeSet::default();
    for idx in 0..2_i64 {
        let iid = 91_000_500 + idx;
        let rec = item_with(&template, iid, 34, char_id, structure_id, InventoryFlag::Hangar, 25, false);
        snap.items.insert(iid, rec.clone());
        neither_changes.items.push(RowChange { key: iid, before: None, after: Some(Arc::new(rec)) });
    }
    let other_rec = item_with(&template, 91_000_510, 34, char_id, 999, InventoryFlag::None, 5, false);
    snap.items.insert(other_rec.item_id, other_rec.clone());
    neither_changes.items.push(RowChange { key: other_rec.item_id, before: None, after: Some(Arc::new(other_rec)) });
    bridge.on_commit(&snap, &neither_changes, &sde, 1_700_000_002, &online, &empty_effects, None);

    let summary = bridge.flush_and_maybe_report(false, None);
    assert_eq!(summary.item_change.matched, 0);
    assert_eq!(summary.item_change.matched_across_scopes, 0);
    assert_eq!(summary.item_change.projected_only, 30);
    assert_eq!(summary.item_change.by_scope.dispatch.sample_projected_only.len(), MAX_COMPARE_SAMPLES);
    assert_eq!(summary.item_change.sample_projected_only.len(), MAX_COMPARE_SAMPLES);

    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=none,after=station,flag=4,owner=character,singleton=true"),
        Some(&20)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=station,after=ship_or_container,flag=5,owner=character,singleton=false"),
        Some(&3)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=ship_or_container,after=recycling_loc_6,flag=0,before_flag=5,owner=character,singleton=false"),
        Some(&2)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=none,after=solar_system,flag=0,owner=character,singleton=true"),
        Some(&2)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=none,after=structure,flag=4,owner=character,singleton=false"),
        Some(&2)
    );
    assert_eq!(
        summary.item_change.projected_only_by_class.get("before=none,after=other,flag=0,owner=character,singleton=false"),
        Some(&1)
    );
    assert_eq!(
        summary.item_change.by_scope.dispatch.projected_only_by_dispatch_method.get("invbroker.Move"),
        Some(&25)
    );
    assert!(summary.item_change.by_scope.tick.projected_only_by_dispatch_method.is_empty());
    assert!(summary.item_change.by_scope.neither.projected_only_by_dispatch_method.is_empty());
}

#[tokio::test]
async fn projected_only_by_dispatch_method_tracks_real_rpc_and_nested_scopes() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));

    let char_id = ctx.db.with_snapshot(|s| *s.characters.keys().next().expect("demo seed character"));
    let station_id = ctx.db.with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let template = ctx.db.with_snapshot(|s| s.items.values().next().expect("demo item").clone());
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);

    let mut sess = SessionState::new(user_id);
    sess.user_id = Some(user_id);
    ctx.db
        .acquire_online_lock(user_id, sess.session_id, None, true, ctx.now_unix())
        .expect("acquire online lock");
    let kw = HashMap::new();

    // Real RPC login brings character online; test also staged/nested RPC scopes with >16 commits.
    let select_res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await;
    assert!(select_res.is_ok(), "SelectCharacterID failed: {select_res:?}");
    assert!(
        ctx.is_character_online(char_id, user_id),
        "SelectCharacterID must mark demo character online"
    );

    let login_summary = flush_item_compare_summary(&ctx);
    assert_eq!(login_summary.item_change.projected_only, 0);
    assert!(login_summary.item_change.projected_only_by_dispatch_method.is_empty());
    ctx.drain_notifications_for_char(char_id);

    // Real end-to-end dispatch_rpc attribution (`dispatch.rs:355-376`):
    // 1. MachoBindObject + piggybacked AssembleCargoContainer exercises nested dispatch_rpc_boxed.
    // 2. Subsequent bound call with wrong nominal `service` exercises `owned_bound_service_for_metrics`.
    let cont_stack_id = 92_100_001_i64;
    let cont_single_id = 92_100_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_stack_id,
            item_with(&template, cont_stack_id, 23, char_id, station_id, InventoryFlag::Hangar, 2, false),
        );
        snap.items.insert(
            cont_single_id,
            item_with(&template, cont_single_id, 23, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
    });
    let seed_summary = flush_item_compare_summary(&ctx);
    assert_eq!(seed_summary.item_change.projected_only, 2);
    assert_eq!(seed_summary.item_change.by_scope.neither.projected_only, 2);
    assert!(seed_summary.item_change.projected_only_by_dispatch_method.is_empty());

    let bind_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(station_id),
                PyRep::Tuple(vec![
                    PyRep::String("AssembleCargoContainer".into()),
                    PyRep::Tuple(vec![PyRep::Integer(cont_stack_id)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("invbroker.MachoBindObject + AssembleCargoContainer must succeed");
    let PyRep::Tuple(bind_parts) = &bind_res else {
        panic!("expected MachoBindObject tuple reply, got {bind_res:?}");
    };
    let bound_id = u64::try_from(
        bind_parts
            .first()
            .and_then(|r| r.as_i64())
            .expect("numeric bound_id from MachoBindObject"),
    )
    .expect("non-negative bound_id");

    let bound_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ignoredServiceAlias",
            Some(bound_id),
            "AssembleCargoContainer",
            &[PyRep::Integer(cont_single_id)],
            &kw,
            None,
        )
        .await;
    assert!(bound_res.is_ok(), "bound AssembleCargoContainer failed: {bound_res:?}");

    let rpc_summary = flush_item_compare_summary(&ctx);
    assert_eq!(rpc_summary.item_change.projected_only, 5);
    assert_eq!(rpc_summary.item_change.by_scope.dispatch.projected_only, 3);
    assert_eq!(
        rpc_summary
            .item_change
            .projected_only_by_dispatch_method
            .get("invbroker.AssembleCargoContainer"),
        Some(&3)
    );
    assert!(
        !rpc_summary
            .item_change
            .projected_only_by_dispatch_method
            .contains_key("ignoredServiceAlias.AssembleCargoContainer")
    );
    ctx.drain_notifications_for_char(char_id);

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let _outer = push_dispatch_rpc_method_guard(
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            normalize_dispatch_method_label("invbroker", "MachoBindObject"),
        );
        for i in 0..18_i64 {
            let _inner = push_dispatch_rpc_method_guard(
                &*ctx.pending_notifications,
                &ctx.notify_tx,
                normalize_dispatch_method_label("invbroker", "Add"),
            );
            let rec = item_with(&template, 92_000_000 + i, 34, char_id, station_id, InventoryFlag::Hangar, 10, false);
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(rec.item_id, rec);
            });
        }
        let outer_rec = item_with(&template, 92_000_100, 34, char_id, station_id, InventoryFlag::Hangar, 5, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(outer_rec.item_id, outer_rec);
        });

        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    // Missing RPC method guard falls back cleanly to "<unknown>".
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        let rec = item_with(&template, 92_000_101, 34, char_id, station_id, InventoryFlag::Hangar, 2, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(rec.item_id, rec);
        });
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "compare-only telemetry must emit zero live item notifications"
    );

    let summary = flush_item_compare_summary(&ctx);
    assert_eq!(summary.item_change.projected_only, 25);
    assert_eq!(summary.item_change.by_scope.dispatch.projected_only, 23);
    assert_eq!(summary.item_change.sample_projected_only.len(), MAX_COMPARE_SAMPLES);
    assert_eq!(
        summary
            .item_change
            .projected_only_by_dispatch_method
            .get("invbroker.AssembleCargoContainer"),
        Some(&3)
    );
    assert_eq!(summary.item_change.projected_only_by_dispatch_method.get("invbroker.Add"), Some(&18));
    assert_eq!(summary.item_change.projected_only_by_dispatch_method.get("invbroker.MachoBindObject"), Some(&1));
    assert_eq!(summary.item_change.projected_only_by_dispatch_method.get("<unknown>"), Some(&1));
    assert_eq!(
        summary.item_change.projected_only_by_dispatch_method,
        summary.item_change.by_scope.dispatch.projected_only_by_dispatch_method
    );
}

#[test]
fn matched_across_scopes_pairs_cleanly_without_distorting_same_scope_matched() {
    let mut ledger = ItemCompareLedger::default();
    let cid = 90_000_055_i64;
    let class_ship = "before=solar_system,after=station,flag=4,owner=character,singleton=true";

    // 1. Commit-first in Tick -> hand-written notify in Dispatch.
    ledger.record_projected_batch(
        CompareOriginScope::Tick,
        None,
        vec![(cid, "itemID", fields_for(5001, Some(587), cid, 60_003_760, 4, -1), class_ship.to_string())],
        Vec::new(),
        0,
    );
    ledger.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "notify_item_record_change",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(5001, Some(587), cid, 60_003_760, 4, -1)),
    );
    let mid = ledger.summary();
    assert_eq!(mid.item_change.matched, 0, "must not inflate same-scope matched");
    assert_eq!(mid.item_change.pending, 0);
    assert_eq!(mid.item_change.hand_written_only, 0);
    assert_eq!(mid.item_change.projected_only, 0);
    assert_eq!(mid.item_change.matched_across_scopes, 1);
    assert_eq!(mid.item_change.matched_across_scopes_by_helper.get("notify_item_record_change"), Some(&1));
    assert_eq!(
        mid.item_change.matched_across_scopes_by_scope_pair.get("projected:tick->hand_written:dispatch"),
        Some(&1)
    );

    // 2. Notify-first in Dispatch -> commit in Neither -> paired on flush & sample/count unwound.
    ledger.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "emit_item_change_to_char",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(5002, Some(34), cid, 60_003_760, 4, 10)),
    );
    assert_eq!(ledger.summary().item_change.hand_written_only, 1);
    assert_eq!(ledger.summary().item_change.sample_hand_written_only.len(), 1);
    ledger.record_projected_batch(
        CompareOriginScope::Neither,
        None,
        vec![(
            cid,
            "itemID",
            fields_for(5002, Some(34), cid, 60_003_760, 4, 10),
            "before=none,after=station,flag=4,owner=character,singleton=false".to_string(),
        )],
        Vec::new(),
        0,
    );
    let after_2 = ledger.flush_pending();
    assert_eq!(after_2.item_change.matched, 0);
    assert_eq!(after_2.item_change.hand_written_only, 0);
    assert!(after_2.item_change.sample_hand_written_only.is_empty());
    assert_eq!(after_2.item_change.projected_only, 0);
    assert_eq!(after_2.item_change.matched_across_scopes, 2);
    assert_eq!(after_2.item_change.matched_across_scopes_by_helper.get("emit_item_change_to_char"), Some(&1));
    assert_eq!(
        after_2.item_change.matched_across_scopes_by_scope_pair.get("projected:neither->hand_written:dispatch"),
        Some(&1)
    );

    // 3. Retired projected_only first (displaced inside Dispatch) -> Tick notify unwinds breakdowns cleanly.
    ledger.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("invbroker.Add".to_string()),
        vec![(cid, "itemID", fields_for(5003, Some(34), cid, 60_003_760, 4, 5), "class_displaced".to_string())],
        Vec::new(),
        0,
    );
    ledger.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("invbroker.Move".to_string()),
        vec![(cid, "itemID", fields_for(5003, Some(34), cid, 60_003_760, 4, 9), "class_live".to_string())],
        Vec::new(),
        0,
    );
    assert_eq!(ledger.summary().item_change.projected_only, 1);
    assert_eq!(ledger.summary().item_change.projected_only_by_class.get("class_displaced"), Some(&1));
    assert_eq!(ledger.summary().item_change.projected_only_by_dispatch_method.get("invbroker.Add"), Some(&1));
    ledger.match_or_record_item_row(
        CompareOriginScope::Tick,
        "tick_helper",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(5003, Some(34), cid, 60_003_760, 4, 9)),
    );
    ledger.match_or_record_item_row(
        CompareOriginScope::Tick,
        "tick_helper",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(5003, Some(34), cid, 60_003_760, 4, 5)),
    );
    let after_3 = ledger.summary();
    assert_eq!(after_3.item_change.projected_only, 0);
    assert!(after_3.item_change.projected_only_by_class.is_empty());
    assert!(after_3.item_change.projected_only_by_dispatch_method.is_empty());
    assert!(after_3.item_change.sample_projected_only.is_empty());
    assert_eq!(after_3.item_change.matched, 0);
    assert_eq!(after_3.item_change.matched_across_scopes, 4);
    assert_eq!(
        after_3.item_change.matched_across_scopes_by_scope_pair.get("projected:dispatch->hand_written:tick"),
        Some(&2)
    );

    // 4. Same-scope priority (both commit orderings for items & blueprints, zero cross-scope eviction)
    //    + same-scope mismatch non-pairing + NDJSON output.
    let mut ledger2 = ItemCompareLedger::default();
    // Ordering A: Tick committed first, then Dispatch committed; both notify in their own scope.
    ledger2.record_projected_batch(
        CompareOriginScope::Tick,
        None,
        vec![(cid, "itemID", fields_for(6001, Some(34), cid, 60_003_760, 4, 1), "c_tick".to_string())],
        vec![(cid, 8001), (cid, 8002)],
        0,
    );
    ledger2.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("stationSvc.Dock".to_string()),
        vec![
            (cid, "itemID", fields_for(6001, Some(34), cid, 60_003_760, 4, 1), "c_disp".to_string()),
            (cid, "itemID", fields_for(6002, Some(34), cid, 60_003_760, 4, 2), "c_mismatch".to_string()),
            (cid, "itemID", fields_for(6003, Some(34), cid, 60_003_760, 4, 3), "c_disp_rev".to_string()),
        ],
        vec![(cid, 8002)],
        0,
    );
    // Ordering B: Tick committed second on same item_id 6003 (must NOT evict Dispatch's 6003).
    ledger2.record_projected_batch(
        CompareOriginScope::Tick,
        None,
        vec![(cid, "itemID", fields_for(6003, Some(34), cid, 60_003_760, 4, 3), "c_tick_rev".to_string())],
        Vec::new(),
        0,
    );
    for (scope, helper, item_id, qty) in [
        (CompareOriginScope::Dispatch, "notify_same_disp", 6001, 1),
        (CompareOriginScope::Tick, "notify_same_tick", 6001, 1),
        (CompareOriginScope::Tick, "notify_same_tick_rev", 6003, 3),
        (CompareOriginScope::Dispatch, "notify_same_disp_rev", 6003, 3),
    ] {
        ledger2.match_or_record_item_row(
            scope,
            helper,
            &[(cid, None)],
            "itemID",
            Some(&fields_for(item_id, Some(34), cid, 60_003_760, 4, qty)),
        );
    }
    ledger2.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "notify_mismatch",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(6002, Some(34), cid, 60_003_760, 4, 99)),
    );
    ledger2.match_or_record_item_row(
        CompareOriginScope::Tick,
        "notify_tick_extra",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(6002, Some(34), cid, 60_003_760, 4, 2)),
    );
    for (scope, helper, owner) in [
        (CompareOriginScope::Dispatch, "notify_bp_cross", 8001),
        (CompareOriginScope::Dispatch, "notify_bp_same_disp", 8002),
        (CompareOriginScope::Tick, "notify_bp_same_tick", 8002),
    ] {
        ledger2.record_hand_written(
            scope,
            None,
            None,
            helper,
            &[(cid, None)],
            "ownerID",
            "OnBlueprintsUpdated",
            &[PyRep::Integer(owner)],
            &[],
        );
    }
    let final_s = ledger2.flush_pending();
    assert_eq!(final_s.item_change.matched, 4);
    assert_eq!(final_s.item_change.matched_across_scopes, 0);
    assert_eq!(final_s.item_change.hand_written_only, 2);
    assert_eq!(final_s.item_change.projected_only, 1);
    assert_eq!(final_s.blueprints_updated.matched, 2);
    assert_eq!(final_s.blueprints_updated.matched_across_scopes, 1);

    let ndjson_path = std::env::temp_dir().join(format!(
        "delta_item_compare_t3a_{}_{:?}.ndjson",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&ndjson_path);
    append_ndjson_record(&ndjson_path, 7, &final_s);
    let written = std::fs::read_to_string(&ndjson_path).expect("read ndjson");
    let _ = std::fs::remove_file(&ndjson_path);
    let parsed: ItemCompareNdjsonRecord = serde_json::from_str(written.trim()).expect("parse ndjson record");
    assert_eq!(parsed.item_change.matched, 4);
    assert_eq!(parsed.item_change.projected_only_by_dispatch_method.get("stationSvc.Dock"), Some(&1));
    assert_eq!(parsed.blueprints_updated.matched, 2);
    assert_eq!(parsed.blueprints_updated.matched_across_scopes, 1);
    assert_eq!(
        parsed.blueprints_updated.matched_across_scopes_by_scope_pair.get("projected:tick->hand_written:dispatch"),
        Some(&1)
    );

    // 5. Negative BF-01 guard: cross-scope field or id_type mismatches must stay unmatched
    //    (pending match, unmatched_projected_items match, and retire_item cross-pairing).
    let mut ledger3 = ItemCompareLedger::default();
    ledger3.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("invbroker.Move".to_string()),
        vec![(cid, "itemID", fields_for(7001, Some(34), cid, 60_003_760, 4, 10), "c_neg1".to_string())],
        Vec::new(),
        0,
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Tick,
        "tick_qty_mismatch",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(7001, Some(34), cid, 60_003_760, 4, 99)),
    );
    ledger3.record_projected_batch(
        CompareOriginScope::Tick,
        None,
        vec![(cid, "itemID", fields_for(7002, Some(587), cid, 60_003_760, 4, -1), "c_neg2".to_string())],
        Vec::new(),
        0,
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "disp_id_type_mismatch",
        &[(cid, None)],
        "shipID",
        Some(&fields_for(7002, Some(587), cid, 60_003_760, 4, -1)),
    );
    ledger3.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("invbroker.Add".to_string()),
        vec![(cid, "itemID", fields_for(7003, Some(34), cid, 60_003_760, 4, 5), "c_neg3".to_string())],
        Vec::new(),
        0,
    );
    ledger3.record_projected_batch(
        CompareOriginScope::Dispatch,
        Some("invbroker.Add".to_string()),
        vec![(cid, "itemID", fields_for(7003, Some(34), cid, 60_003_760, 4, 6), "c_neg3b".to_string())],
        Vec::new(),
        0,
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "disp_clear_7003",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(7003, Some(34), cid, 60_003_760, 4, 6)),
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Tick,
        "tick_retired_field_mismatch",
        &[(cid, None)],
        "itemID",
        Some(&fields_for(7003, Some(34), cid, 60_003_760, 4, 7)),
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Tick,
        "tick_retired_id_type_mismatch",
        &[(cid, None)],
        "shipID",
        Some(&fields_for(7003, Some(34), cid, 60_003_760, 4, 5)),
    );
    ledger3.match_or_record_item_row(
        CompareOriginScope::Dispatch,
        "disp_first_id_type_mismatch",
        &[(cid, None)],
        "shipID",
        Some(&fields_for(7004, Some(587), cid, 60_003_760, 4, -1)),
    );
    ledger3.record_projected_batch(
        CompareOriginScope::Tick,
        None,
        vec![(cid, "itemID", fields_for(7004, Some(587), cid, 60_003_760, 4, -1), "c_neg4".to_string())],
        Vec::new(),
        0,
    );
    let neg_s = ledger3.flush_pending();
    assert_eq!(neg_s.item_change.matched, 1);
    assert_eq!(neg_s.item_change.matched_across_scopes, 0);
    assert_eq!(neg_s.item_change.projected_only, 4);
    assert_eq!(neg_s.item_change.hand_written_only, 5);
}

#[test]
fn compare_change_dict_key_order_independent_and_differs_by_helper() {
    let cid = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut ledger = ItemCompareLedger::default();
    let proj_dict_a = CanonicalChangeDict::from_entries(&[
        (3, PyRep::Integer(station_id)),
        (4, PyRep::Integer(InventoryFlag::Hangar as i64)),
    ]);
    let proj_dict_b = CanonicalChangeDict::from_entries(&[
        (3, PyRep::Integer(station_id)),
        (4, PyRep::Integer(InventoryFlag::Cargo as i64)),
        (9, PyRep::Integer(10)),
    ]);
    let proj_dict_cross = CanonicalChangeDict::from_entries(&[(10, PyRep::Integer(25))]);

    let mut proj_a = fields_for(8_100_001, Some(34), cid, station_id, 5, 50);
    proj_a.change_dict = Some(proj_dict_a);
    let mut proj_b = fields_for(8_100_002, Some(34), cid, 6, 0, 0);
    proj_b.change_dict = Some(proj_dict_b);
    let mut proj_c = fields_for(8_100_003, Some(34), cid, station_id, 4, 25);
    proj_c.change_dict = Some(proj_dict_cross);

    ledger.record_projected_batch_detailed(
        CompareOriginScope::Dispatch,
        Some("invbroker.Move".to_string()),
        None,
        None,
        vec![
            (
                cid,
                "charid",
                proj_a,
                "before=station,after=station,flag=5,owner=character,singleton=false".into(),
                true,
            ),
            (
                cid,
                "charid",
                proj_b,
                "before=ship_or_container,after=recycling_loc_6,flag=0,before_flag=5,owner=character,singleton=false".into(),
                true,
            ),
            (
                cid,
                "charid",
                proj_c,
                "before=station,after=station,flag=4,owner=character,singleton=false".into(),
                true,
            ),
        ],
        Vec::new(),
        0,
    );

    // 1. Key-reversed change dictionary {4, 3} matches {3, 4}.
    let mut hw_a = fields_for(8_100_001, Some(34), cid, station_id, 5, 50);
    hw_a.change_dict = Some(CanonicalChangeDict::from_entries(&[
        (4, PyRep::Integer(InventoryFlag::Hangar as i64)),
        (3, PyRep::Integer(station_id)),
    ]));
    ledger.match_or_record_item_row_detailed(
        CompareOriginScope::Dispatch,
        None,
        Some("invbroker.Move"),
        "notify_item_mutation",
        &[(cid, None)],
        "charid",
        Some(&hw_a),
        None,
    );

    // 2. Deleted item where helper only emits {3: old_loc} without {4, 9} records dictionary_differs
    //    AND must NOT drop `first_set` even when legacy `preclassified` reports `hw_fs = false` (BF-01).
    let mut hw_b = fields_for(8_100_002, Some(34), cid, 6, 0, 0);
    hw_b.change_dict = Some(CanonicalChangeDict::from_entries(&[(
        3,
        PyRep::Integer(station_id),
    )]));
    ledger.match_or_record_item_row_detailed(
        CompareOriginScope::Dispatch,
        None,
        Some("invbroker.Move"),
        "TrashItems",
        &[(cid, None)],
        "charid",
        Some(&hw_b),
        Some((
            "before=ship_or_container,after=recycling_loc_6,flag=0,before_flag=5,owner=character,singleton=false",
            false,
        )),
    );

    // 3. Cross-scope match also compares change dictionaries cleanly.
    let mut hw_c = fields_for(8_100_003, Some(34), cid, station_id, 4, 25);
    hw_c.change_dict = Some(CanonicalChangeDict::from_entries(&[(
        10,
        PyRep::Integer(99),
    )]));
    ledger.match_or_record_item_row_detailed(
        CompareOriginScope::Tick,
        None,
        None,
        "tick_helper",
        &[(cid, None)],
        "charid",
        Some(&hw_c),
        None,
    );

    let s = ledger.flush_pending();
    assert_eq!(s.item_change.matched, 2);
    assert_eq!(s.item_change.matched_across_scopes, 1);
    assert_eq!(s.item_change.dictionary_same, 1);
    assert_eq!(s.item_change.dictionary_differs, 2);
    assert_eq!(s.item_change.first_set.matched, 2);
    assert_eq!(s.item_change.first_set.dictionary_same, 1);
    assert_eq!(s.item_change.first_set.dictionary_differs, 1);
    assert_eq!(
        s.item_change.dictionary_by_helper.get("notify_item_mutation"),
        Some(&HelperDictionaryCounts {
            same_dictionary: 1,
            different_dictionary: 0,
        })
    );
    assert_eq!(
        s.item_change.dictionary_by_helper.get("TrashItems"),
        Some(&HelperDictionaryCounts {
            same_dictionary: 0,
            different_dictionary: 1,
        })
    );
    assert_eq!(
        s.item_change.dictionary_by_helper.get("tick_helper"),
        Some(&HelperDictionaryCounts {
            same_dictionary: 0,
            different_dictionary: 1,
        })
    );
    assert_eq!(s.item_change.sample_dictionary_differs.len(), 2);
}

#[tokio::test]
async fn compare_section4_first_set_contexts_hand_written_breakdowns_and_ndjson() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));
    let ndjson_path = std::env::temp_dir().join(format!(
        "evomu_delta_item_compare_sec4_{}_{}.ndjson",
        std::process::id(),
        ctx.now_unix()
    ));
    let _ = std::fs::remove_file(&ndjson_path);
    set_item_compare_override_for_test(&ctx, Some(true), Some(Some(ndjson_path.clone())));

    let char_id = ctx.db.with_snapshot(|s| *s.characters.keys().next().expect("demo seed char"));
    let station_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let template = ctx
        .db
        .with_snapshot(|s| s.items.values().next().expect("demo item").clone());
    let ship_id = 91_800_010_i64;
    let move_id = 91_800_020_i64;
    let drone_id = 91_800_021_i64;
    let del_cargo_id = 91_800_022_i64;
    let hw_only_hangar_id = 91_800_023_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            item_with(&template, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            move_id,
            item_with(&template, move_id, 34, char_id, station_id, InventoryFlag::Hangar, 40, false),
        );
        snap.items.insert(
            del_cargo_id,
            item_with(&template, del_cargo_id, 34, char_id, ship_id, InventoryFlag::Cargo, 15, false),
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
            c.active_ship_id = ship_id;
        }
    });
    ctx.db
        .acquire_online_lock(user_id, 93_140, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut before_move = None;
    let mut after_move = None;
    let mut before_del = None;
    let mut drone_after = None;

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("sec4_dispatch_mix", |tx| -> Result<(), eve_db::TxError> {
                let prev_move = tx.snap.items.get(&move_id).expect("move item").clone();
                let mut next_move = prev_move.clone();
                next_move.location_id = ship_id;
                next_move.flag = InventoryFlag::Cargo;
                tx.snap.items.insert(move_id, next_move.clone());
                before_move = Some(prev_move);
                after_move = Some(next_move);

                let drone = item_with(
                    &template,
                    drone_id,
                    2454,
                    char_id,
                    ship_id,
                    InventoryFlag::DroneBay,
                    5,
                    false,
                );
                tx.snap.items.insert(drone_id, drone.clone());
                drone_after = Some(drone);

                before_del = tx.snap.remove_system_cleanup_item(del_cargo_id);
                Ok(())
            })
            .expect("tx");

        // Matched first-set move (4 -> 5), matched non-first-set DroneBay (flag 87),
        // and matched first-set deletion (before_flag=5) via Tombstone.
        ctx.notify_item_mutation(char_id, before_move.as_ref(), after_move.as_ref());
        ctx.notify_item_mutation(char_id, None, drone_after.as_ref());
        ctx.notify_item_mutation(char_id, before_del.as_ref(), None);

        // Standalone hand-written-only notify in first-set hangar flag 4 without DB item change.
        let hw_rec = item_with(
            &template,
            hw_only_hangar_id,
            34,
            char_id,
            station_id,
            InventoryFlag::Hangar,
            9,
            false,
        );
        ctx.notify_item_mutation(char_id, None, Some(&hw_rec));
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    log_item_compare_summary(&ctx);
    let summary = item_compare_summary(&ctx);
    let ic = &summary.item_change;

    let hangar_to_cargo = "before=station,after=ship_or_container,flag=5,owner=character,singleton=false";
    let cargo_del = "before=ship_or_container,after=recycling_loc_6,flag=0,before_flag=5,owner=character,singleton=false";
    let drone_bay = "before=none,after=ship_or_container,flag=87,owner=character,singleton=false";
    let hw_hangar = "before=none,after=station,flag=4,owner=character,singleton=false";

    assert_eq!(ic.matched, 3, "ic={ic:#?}");
    assert_eq!(ic.matched_by_class.get(hangar_to_cargo), Some(&1));
    assert_eq!(ic.matched_by_class.get(cargo_del), Some(&1));
    assert_eq!(ic.matched_by_class.get(drone_bay), Some(&1));
    assert_eq!(
        ic.matched_by_dispatch_method.get(crate::flight_recorder::UNKNOWN_RPC_LABEL),
        Some(&3)
    );

    assert_eq!(ic.hand_written_only, 1, "ic={ic:#?}");
    assert_eq!(ic.hand_written_only_by_class.get(hw_hangar), Some(&1));
    assert_eq!(
        ic.hand_written_only_by_dispatch_method
            .get(crate::flight_recorder::UNKNOWN_RPC_LABEL),
        Some(&1)
    );

    // First-set excludes DroneBay (flag 87) and keeps hangar/cargo move, deletion (before_flag=5),
    // and hand-written hangar item.
    assert_eq!(ic.first_set.matched, 2, "first_set={:#?}", ic.first_set);
    assert_eq!(ic.first_set.matched_by_class.get(hangar_to_cargo), Some(&1));
    assert_eq!(ic.first_set.matched_by_class.get(cargo_del), Some(&1));
    assert_eq!(ic.first_set.matched_by_class.get(drone_bay), None);
    assert_eq!(ic.first_set.hand_written_only, 1);
    assert_eq!(ic.first_set.hand_written_only_by_class.get(hw_hangar), Some(&1));
    assert_eq!(ic.contexts_by_class.get(hangar_to_cargo), Some(&1));
    assert_eq!(ic.contexts_by_class.get(cargo_del), Some(&1));
    assert_eq!(ic.contexts_by_class.get(drone_bay), Some(&1));
    assert_eq!(ic.contexts_by_class.get(hw_hangar), Some(&1));
    assert_eq!(ic.first_set.contexts_by_class.get(hangar_to_cargo), Some(&1));
    assert_eq!(ic.first_set.contexts_by_class.get(drone_bay), None);

    let ndjson_raw = std::fs::read_to_string(&ndjson_path).expect("read ndjson");
    let _ = std::fs::remove_file(&ndjson_path);
    let parsed: serde_json::Value = serde_json::from_str(
        ndjson_raw.lines().last().expect("ndjson line"),
    )
    .expect("valid json");
    assert_eq!(parsed["item_change"]["first_set"]["matched"], 2);
}

#[tokio::test]
async fn bf01_first_set_rejects_owner_transfer_and_missing_dict_allows_empty_insert() {
    let ctx = ServerContext::new();
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));

    let char_id = ctx.db.with_snapshot(|s| *s.characters.keys().next().expect("demo seed char"));
    let station_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let template = ctx
        .db
        .with_snapshot(|s| s.items.values().next().expect("demo item").clone());
    let other_owner_id = 90_999_001_i64;
    let transfer_item_id = 91_810_001_i64;
    let standalone_transfer_id = 91_810_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
        snap.items.insert(
            transfer_item_id,
            item_with(
                &template,
                transfer_item_id,
                34,
                other_owner_id,
                station_id,
                InventoryFlag::Hangar,
                25,
                false,
            ),
        );
    });
    ctx.db
        .acquire_online_lock(user_id, 93_150, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let snap = ctx.db.with_snapshot(|s| s.clone());
    let changed_chars = HashMap::new();
    let old_owner_rec = item_with(
        &template,
        transfer_item_id,
        34,
        other_owner_id,
        station_id,
        InventoryFlag::Hangar,
        25,
        false,
    );
    let new_owner_rec = item_with(
        &template,
        transfer_item_id,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        25,
        false,
    );
    let fp_hangar = ItemWireFingerprint {
        owner_id: char_id,
        location_id: station_id,
        flag_id: InventoryFlag::Hangar as i64,
        wire_qty: 25,
    };

    // Projected path rejects owner transfer even when before/after stay in docked hangar flag 4.
    assert_eq!(
        is_first_set_item_change_pair(
            true,
            char_id,
            Some(&old_owner_rec),
            Some(&new_owner_rec),
            &snap,
            &changed_chars,
            &*ctx.db,
            &ctx.sde,
        ),
        None
    );

    // Hand-written path rejects `{2: other_owner_id}` in docked hangar flag 4.
    let owner_xfer_dict = CanonicalChangeDict::from_entries(&[(2, PyRep::Integer(other_owner_id))]);
    assert_eq!(
        evaluate_hand_written_first_set_in_snap(
            true,
            char_id,
            1,
            ProjectedItemOpKind::CreatedOrUpdated,
            fp_hangar,
            Some(&owner_xfer_dict),
            &snap,
            &*ctx.db,
            &ctx.sde,
        ),
        None
    );

    // `change_dict == None` is rejected for both CreatedOrUpdated and Deleted.
    assert_eq!(
        evaluate_hand_written_first_set_in_snap(
            true,
            char_id,
            1,
            ProjectedItemOpKind::CreatedOrUpdated,
            fp_hangar,
            None,
            &snap,
            &*ctx.db,
            &ctx.sde,
        ),
        None
    );
    assert_eq!(
        evaluate_hand_written_first_set_in_snap(
            true,
            char_id,
            1,
            ProjectedItemOpKind::Deleted,
            ItemWireFingerprint {
                owner_id: char_id,
                location_id: 6,
                flag_id: 0,
                wire_qty: 0,
            },
            None,
            &snap,
            &*ctx.db,
            &ctx.sde,
        ),
        None
    );

    // Empty `{}` insert payload parses as `Some(empty)` and classifies as first-set,
    // while missing/unparseable dict args yield `change_dict == None`.
    let hangar_row = PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(standalone_transfer_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("ownerID".into()), PyRep::Integer(char_id)),
        (PyRep::String("locationID".into()), PyRep::Integer(station_id)),
        (PyRep::String("flagID".into()), PyRep::Integer(InventoryFlag::Hangar as i64)),
        (PyRep::String("quantity".into()), PyRep::Integer(10)),
    ]);
    let empty_dict_fields =
        extract_item_change_fields(&[hangar_row.clone(), PyRep::Dict(Vec::new())])
            .expect("fields with empty dict");
    assert!(empty_dict_fields.change_dict.as_ref().is_some_and(|d| d.0.is_empty()));
    assert!(
        evaluate_hand_written_first_set_in_snap(
            true,
            char_id,
            1,
            ProjectedItemOpKind::CreatedOrUpdated,
            fp_hangar,
            empty_dict_fields.change_dict.as_ref(),
            &snap,
            &*ctx.db,
            &ctx.sde,
        )
        .is_some()
    );
    assert!(
        extract_item_change_fields(&[hangar_row.clone()])
            .expect("fields without dict")
            .change_dict
            .is_none()
    );
    assert!(
        extract_item_change_fields(&[hangar_row.clone(), PyRep::None])
            .expect("fields with None dict")
            .change_dict
            .is_none()
    );

    // End-to-end inside dispatch: matched owner transfer in hangar + standalone `{2: other_owner_id}`
    // must keep `first_set.matched == 0` and `first_set.hand_written_only == 0`.
    let mut pre_xfer = None;
    let mut post_xfer = None;
    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        ctx.db
            .tx("bf01_owner_xfer_in_hangar", |tx| -> Result<(), eve_db::TxError> {
                let prev = tx.snap.items.get(&transfer_item_id).expect("transfer item").clone();
                let mut next = prev.clone();
                next.owner_id = char_id;
                tx.snap.items.insert(transfer_item_id, next.clone());
                pre_xfer = Some(prev);
                post_xfer = Some(next);
                Ok(())
            })
            .expect("tx");
        ctx.notify_item_mutation(char_id, pre_xfer.as_ref(), post_xfer.as_ref());

        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![
                hangar_row,
                PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Integer(other_owner_id))]),
            ],
        );
        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let summary = flush_item_compare_summary(&ctx);
    assert_eq!(summary.item_change.matched, 1, "{summary:#?}");
    assert_eq!(summary.item_change.hand_written_only, 1, "{summary:#?}");
    assert_eq!(summary.item_change.projected_only, 0, "{summary:#?}");
    assert_eq!(summary.item_change.first_set.matched, 0, "{summary:#?}");
    assert_eq!(summary.item_change.first_set.hand_written_only, 0, "{summary:#?}");
    assert_eq!(summary.item_change.first_set.projected_only, 0, "{summary:#?}");
}

#[tokio::test]
async fn bf02_multi_commit_dispatch_transition_invalidates_prior_pending_and_matched_first_set() {
    // Part A: Commit 1 mutates docked hangar item (`first_set=true`), Commit 2 changes active_ship_id
    // in the same dispatch scope, then hand-written notify arrives -> `matched=1`, `first_set.matched=0`.
    let ctx_matched = ServerContext::new();
    set_item_compare_override_for_test(&ctx_matched, Some(true), Some(None));

    let char_id = ctx_matched
        .db
        .with_snapshot(|s| *s.characters.keys().next().expect("demo seed char"));
    let station_id = ctx_matched
        .db
        .with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let user_id = ctx_matched.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let template = ctx_matched
        .db
        .with_snapshot(|s| s.items.values().next().expect("demo item").clone());
    let hangar_item_a = 91_820_001_i64;
    let new_ship_id = 91_820_010_i64;

    ctx_matched.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
    });
    ctx_matched
        .db
        .acquire_online_lock(user_id, 93_151, Some(char_id), true, ctx_matched.now_unix())
        .expect("online lock");

    let item_a = item_with(
        &template,
        hangar_item_a,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        15,
        false,
    );
    with_staged_dispatch_notifications(
        &*ctx_matched.pending_notifications,
        &ctx_matched.notify_tx,
        async {
            ctx_matched
                .db
                .tx("bf02_commit1_hangar_insert", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(hangar_item_a, item_a.clone());
                    Ok(())
                })
                .expect("commit1");
            ctx_matched
                .db
                .tx("bf02_commit2_ship_transition", |tx| -> Result<(), eve_db::TxError> {
                    let ch = tx.snap.characters.get_mut(&char_id).expect("char");
                    ch.active_ship_id = new_ship_id;
                    Ok(())
                })
                .expect("commit2");
            ctx_matched.notify_item_mutation(char_id, None, Some(&item_a));
            flush_staged_dispatch_notifications(
                &*ctx_matched.pending_notifications,
                &ctx_matched.notify_tx,
            );
        },
    )
    .await;

    let summary_matched = flush_item_compare_summary(&ctx_matched);
    assert_eq!(summary_matched.item_change.matched, 1, "{summary_matched:#?}");
    assert_eq!(
        summary_matched.item_change.first_set.matched, 0,
        "Commit 2 transition must clear Commit 1 first_set before match: {summary_matched:#?}"
    );

    // Part B: Same two-commit sequence without hand-written notify -> retiring Commit 1 pending
    // item increments `projected_only` without incrementing `first_set.projected_only`.
    let ctx_retired = ServerContext::new();
    set_item_compare_override_for_test(&ctx_retired, Some(true), Some(None));
    let item_b_id = 91_820_002_i64;
    ctx_retired.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
    });
    ctx_retired
        .db
        .acquire_online_lock(user_id, 93_152, Some(char_id), true, ctx_retired.now_unix())
        .expect("online lock");

    let item_b = item_with(
        &template,
        item_b_id,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        20,
        false,
    );
    with_staged_dispatch_notifications(
        &*ctx_retired.pending_notifications,
        &ctx_retired.notify_tx,
        async {
            ctx_retired
                .db
                .tx("bf02_commit1_hangar_retired", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(item_b_id, item_b.clone());
                    Ok(())
                })
                .expect("commit1");
            ctx_retired
                .db
                .tx("bf02_commit2_station_transition", |tx| -> Result<(), eve_db::TxError> {
                    let ch = tx.snap.characters.get_mut(&char_id).expect("char");
                    ch.station_id = Some(station_id + 1);
                    Ok(())
                })
                .expect("commit2");
        },
    )
    .await;

    let summary_retired = flush_item_compare_summary(&ctx_retired);
    assert_eq!(summary_retired.item_change.projected_only, 1, "{summary_retired:#?}");
    assert_eq!(
        summary_retired.item_change.first_set.projected_only, 0,
        "Commit 2 transition must clear Commit 1 first_set before retire: {summary_retired:#?}"
    );

    // Part C: `match_or_record_item_row_detailed` must reject `first_set` when
    // `recipients.len() != 1`, while DB-authoritative `matched_proj.first_set == true`
    // single-recipient matches remain counted even if `preclassified` says `hw_fs == false` (BF-01).
    let mut ledger = ItemCompareLedger::default();
    let cid2 = char_id + 1;
    let f_multi = fields_for(8_200_001, Some(34), char_id, station_id, 4, 10);
    let f_hw_false = fields_for(8_200_002, Some(34), char_id, station_id, 4, 20);
    let class_key = "before=station,after=station,flag=4,owner=character,singleton=false";
    ledger.record_projected_batch_detailed(
        CompareOriginScope::Dispatch,
        Some("invbroker.Move".to_string()),
        Some(77),
        None,
        vec![
            (char_id, "charid", f_multi.clone(), class_key.into(), true),
            (char_id, "charid", f_hw_false.clone(), class_key.into(), true),
        ],
        Vec::new(),
        0,
    );
    ledger.match_or_record_item_row_detailed(
        CompareOriginScope::Dispatch,
        Some(77),
        Some("invbroker.Move"),
        "notify_item_mutation",
        &[(char_id, None), (cid2, None)],
        "charid",
        Some(&f_multi),
        Some((class_key, true)),
    );
    ledger.match_or_record_item_row_detailed(
        CompareOriginScope::Dispatch,
        Some(77),
        Some("invbroker.Move"),
        "notify_item_mutation",
        &[(char_id, None)],
        "charid",
        Some(&f_hw_false),
        Some((class_key, false)),
    );
    let ledger_summary = ledger.flush_pending();
    assert_eq!(ledger_summary.item_change.matched, 2, "{ledger_summary:#?}");
    assert_eq!(
        ledger_summary.item_change.first_set.matched, 1,
        "multi-recipient must be excluded while DB-authoritative single-recipient match must not be gated on hw_fs: {ledger_summary:#?}"
    );
}

fn make_item_row_dict(
    item_id: i64,
    type_id: i64,
    owner_id: i64,
    location_id: i64,
    flag_id: i64,
    quantity: i64,
) -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(type_id)),
        (PyRep::String("ownerID".into()), PyRep::Integer(owner_id)),
        (
            PyRep::String("locationID".into()),
            PyRep::Integer(location_id),
        ),
        (PyRep::String("flagID".into()), PyRep::Integer(flag_id)),
        (PyRep::String("quantity".into()), PyRep::Integer(quantity)),
    ])
}

#[tokio::test]
async fn r2_bf01_matched_first_set_not_dropped_by_malformed_or_partial_hand_written_dict() {
    let ctx = ServerContext::new();
    // Pause compare while seeding fixture rows outside dispatch so `neither` scope stays clean.
    set_item_compare_override_for_test(&ctx, Some(false), Some(None));

    let char_id = ctx
        .db
        .with_snapshot(|s| *s.characters.keys().next().expect("demo seed char"));
    let station_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let template = ctx
        .db
        .with_snapshot(|s| s.items.values().next().expect("demo item").clone());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
    });
    ctx.db
        .acquire_online_lock(user_id, 93_161, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let del_item_id = 91_830_001_i64;
    let mod_item_id = 91_830_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            del_item_id,
            item_with(
                &template,
                del_item_id,
                34,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                12,
                false,
            ),
        );
        snap.items.insert(
            mod_item_id,
            item_with(
                &template,
                mod_item_id,
                34,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                10,
                false,
            ),
        );
    });
    set_item_compare_override_for_test(&ctx, Some(true), Some(None));

    with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
        // Case 1: First-set deletion where legacy helper emits only `{3: old_loc}` (omitting 4, 9).
        ctx.db
            .tx("r2_bf01_delete_hangar_item", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.remove(&del_item_id);
                Ok(())
            })
            .expect("delete tx");
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![
                make_item_row_dict(del_item_id, 34, char_id, 6, 0, 0),
                PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(station_id))]),
            ],
        );

        // Case 2: First-set hangar mutation where legacy helper emits `PyRep::None` as change dict.
        ctx.db
            .tx("r2_bf01_modify_hangar_item", |tx| -> Result<(), eve_db::TxError> {
                let item = tx.snap.items.get_mut(&mod_item_id).expect("mod item");
                item.quantity = 25;
                Ok(())
            })
            .expect("modify tx");
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![
                make_item_row_dict(
                    mod_item_id,
                    34,
                    char_id,
                    station_id,
                    InventoryFlag::Hangar as i64,
                    25,
                ),
                PyRep::None,
            ],
        );

        flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
    })
    .await;

    let summary = flush_item_compare_summary(&ctx);
    assert_eq!(summary.item_change.matched, 2, "{summary:#?}");
    assert_eq!(summary.item_change.hand_written_only, 0, "{summary:#?}");
    assert_eq!(summary.item_change.projected_only, 0, "{summary:#?}");
    assert_eq!(
        summary.item_change.first_set.matched, 2,
        "DB-authoritative first-set matches must survive partial/None legacy dicts: {summary:#?}"
    );
    assert_eq!(
        summary.item_change.first_set.dictionary_differs, 2,
        "dictionary mismatch on both partial-delete and None-dict matches must report in first_set: {summary:#?}"
    );
    assert_eq!(summary.item_change.first_set.dictionary_same, 0, "{summary:#?}");
}

#[tokio::test]
async fn r2_bf02_commit1_notify_retire_before_commit2_transition_rolls_back_first_set() {
    // Subcase A: Commit 1 (hangar item) -> Notify 1 (immediate match) -> Commit 2 (active_ship_id transition).
    let ctx_matched = ServerContext::new();
    set_item_compare_override_for_test(&ctx_matched, Some(true), Some(None));

    let char_id = ctx_matched
        .db
        .with_snapshot(|s| *s.characters.keys().next().expect("demo seed char"));
    let station_id = ctx_matched
        .db
        .with_snapshot(|s| s.characters[&char_id].station_id.unwrap_or(60_003_760));
    let user_id = ctx_matched
        .db
        .with_snapshot(|s| s.characters[&char_id].user_id);
    let template = ctx_matched
        .db
        .with_snapshot(|s| s.items.values().next().expect("demo item").clone());

    ctx_matched.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
    });
    ctx_matched
        .db
        .acquire_online_lock(user_id, 93_162, Some(char_id), true, ctx_matched.now_unix())
        .expect("online lock");

    let hangar_item_a = 91_830_101_i64;
    let new_ship_id = 91_830_110_i64;
    let item_a = item_with(
        &template,
        hangar_item_a,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        15,
        false,
    );

    with_staged_dispatch_notifications(
        &*ctx_matched.pending_notifications,
        &ctx_matched.notify_tx,
        async {
            ctx_matched
                .db
                .tx("r2_bf02_commit1_hangar_insert", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(hangar_item_a, item_a.clone());
                    Ok(())
                })
                .expect("commit1");
            // Immediate notification BEFORE Commit 2 transitions the pilot:
            ctx_matched.notify_item_mutation(char_id, None, Some(&item_a));

            let mid = item_compare_summary(&ctx_matched);
            assert_eq!(mid.item_change.matched, 1, "{mid:#?}");
            assert_eq!(mid.item_change.first_set.matched, 1, "{mid:#?}");
            assert_eq!(mid.item_change.first_set.dictionary_same, 1, "{mid:#?}");
            assert!(
                !mid.item_change.first_set.matched_by_class.is_empty(),
                "{mid:#?}"
            );

            // Commit 2 mutates active_ship_id in the same dispatch scope:
            ctx_matched
                .db
                .tx("r2_bf02_commit2_ship_transition", |tx| -> Result<(), eve_db::TxError> {
                    let ch = tx.snap.characters.get_mut(&char_id).expect("char");
                    ch.active_ship_id = new_ship_id;
                    Ok(())
                })
                .expect("commit2");

            flush_staged_dispatch_notifications(
                &*ctx_matched.pending_notifications,
                &ctx_matched.notify_tx,
            );
        },
    )
    .await;

    let summary_matched = flush_item_compare_summary(&ctx_matched);
    assert_eq!(summary_matched.item_change.matched, 1, "{summary_matched:#?}");
    assert_eq!(
        summary_matched.item_change.dictionary_same, 1,
        "{summary_matched:#?}"
    );
    assert_eq!(
        summary_matched.item_change.first_set.matched, 0,
        "Commit 2 transition must roll back Commit 1 -> Notify 1 first_set.matched: {summary_matched:#?}"
    );
    assert_eq!(
        summary_matched.item_change.first_set.dictionary_same, 0,
        "{summary_matched:#?}"
    );
    assert_eq!(
        summary_matched.item_change.first_set.dictionary_differs, 0,
        "{summary_matched:#?}"
    );
    assert!(
        summary_matched
            .item_change
            .first_set
            .matched_by_class
            .is_empty(),
        "{summary_matched:#?}"
    );
    assert!(
        summary_matched
            .item_change
            .first_set
            .matched_by_dispatch_method
            .is_empty(),
        "{summary_matched:#?}"
    );

    // Subcase B: Commit 1 (hangar item) -> early retire via overwrite Commit 1b + standalone
    // HandWrittenOnly notify -> Commit 2 (station_id transition) rolls back both `projected_only`
    // and `hand_written_only` first_set buckets.
    let ctx_retired = ServerContext::new();
    set_item_compare_override_for_test(&ctx_retired, Some(true), Some(None));
    ctx_retired.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
            c.structure_id = None;
        }
    });
    ctx_retired
        .db
        .acquire_online_lock(user_id, 93_163, Some(char_id), true, ctx_retired.now_unix())
        .expect("online lock");

    let item_b_id = 91_830_201_i64;
    let hw_extra_id = 91_830_202_i64;
    let item_b = item_with(
        &template,
        item_b_id,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        20,
        false,
    );

    with_staged_dispatch_notifications(
        &*ctx_retired.pending_notifications,
        &ctx_retired.notify_tx,
        async {
            ctx_retired
                .db
                .tx("r2_bf02_commit1_hangar", |tx| -> Result<(), eve_db::TxError> {
                    tx.snap.items.insert(item_b_id, item_b.clone());
                    Ok(())
                })
                .expect("commit1");
            // Overwrite same item in Commit 1b BEFORE Commit 2 retires Commit 1's pending entry early:
            ctx_retired
                .db
                .tx("r2_bf02_commit1b_overwrite", |tx| -> Result<(), eve_db::TxError> {
                    let row = tx.snap.items.get_mut(&item_b_id).expect("item_b");
                    row.quantity = 25;
                    Ok(())
                })
                .expect("commit1b");
            // Standalone HandWrittenOnly notification before Commit 2 transition:
            ctx_retired.notify(
                NotificationTarget::Character(char_id),
                "OnItemChange",
                "charid",
                vec![
                    make_item_row_dict(
                        hw_extra_id,
                        34,
                        char_id,
                        station_id,
                        InventoryFlag::Hangar as i64,
                        5,
                    ),
                    PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
                ],
            );

            let mid = item_compare_summary(&ctx_retired);
            assert_eq!(mid.item_change.projected_only, 1, "{mid:#?}");
            assert_eq!(mid.item_change.first_set.projected_only, 1, "{mid:#?}");
            assert_eq!(mid.item_change.hand_written_only, 1, "{mid:#?}");
            assert_eq!(mid.item_change.first_set.hand_written_only, 1, "{mid:#?}");

            // Commit 2 mutates station_id in the same dispatch scope:
            ctx_retired
                .db
                .tx("r2_bf02_commit2_station_transition", |tx| -> Result<(), eve_db::TxError> {
                    let ch = tx.snap.characters.get_mut(&char_id).expect("char");
                    ch.station_id = Some(station_id + 1);
                    Ok(())
                })
                .expect("commit2");

            flush_staged_dispatch_notifications(
                &*ctx_retired.pending_notifications,
                &ctx_retired.notify_tx,
            );
        },
    )
    .await;

    let summary_retired = flush_item_compare_summary(&ctx_retired);
    assert_eq!(summary_retired.item_change.projected_only, 2, "{summary_retired:#?}");
    assert_eq!(summary_retired.item_change.hand_written_only, 1, "{summary_retired:#?}");
    assert_eq!(
        summary_retired.item_change.first_set.projected_only, 0,
        "Commit 2 transition must roll back retired Commit 1 first_set.projected_only: {summary_retired:#?}"
    );
    assert_eq!(
        summary_retired.item_change.first_set.hand_written_only, 0,
        "Commit 2 transition must roll back Commit 1 first_set.hand_written_only: {summary_retired:#?}"
    );
    assert!(
        summary_retired
            .item_change
            .first_set
            .projected_only_by_class
            .is_empty(),
        "{summary_retired:#?}"
    );
    assert!(
        summary_retired
            .item_change
            .first_set
            .hand_written_only_by_class
            .is_empty(),
        "{summary_retired:#?}"
    );
}
