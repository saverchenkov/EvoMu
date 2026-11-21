//! Unit tests for `#931` Track 3A compare-only telemetry & cross-scope pairing.

use super::*;
use crate::router::cache_notifications::{
    flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
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
        summary.item_change.projected_only_by_class.get("before=ship_or_container,after=recycling_loc_6,flag=0,owner=character,singleton=false"),
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
            helper,
            &[(cid, None)],
            "ownerID",
            "OnBlueprintsUpdated",
            &[PyRep::Integer(owner)],
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
