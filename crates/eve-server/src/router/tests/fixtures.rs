//! Shared test fixtures, expansion assets, and test session helpers.

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

pub(crate) fn seed_test_expansion_assets(ctx: &ServerContext) {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    ctx.db.with_mut_snapshot(|snap| {
        let extra_items = [
            (
                100_000_013,
                47746,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_014,
                40520,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
            (
                100_000_015,
                35832,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_016,
                56201,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_017,
                35814,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_019,
                47763,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_020,
                20424,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_028,
                20172,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_023,
                23055,
                100_000_024,
                InventoryFlag::FighterBay,
                9,
                false,
            ),
            (
                100_000_024,
                24483,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                true,
            ),
            (
                100_000_025,
                4247,
                100_000_024,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_026,
                4247,
                100_000_001,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_018,
                1230,
                60_003_760,
                InventoryFlag::Hangar,
                5_000,
                false,
            ),
            (
                100_000_027,
                34748,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
            // Spare Rifter (100_000_029) in Jita 4-4 hangar with fitted autocannon for post-CONCORD kill-right engagement
            (100_000_029, 587, 60_003_760, InventoryFlag::Hangar, 1, true),
            (
                100_000_030,
                2889,
                100_000_029,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
            // Third Rifter (100_000_035) in Jita 4-4 hangar carrying structure/filament supplies for post-PvP ship loss
            (100_000_035, 587, 60_003_760, InventoryFlag::Hangar, 1, true),
            (
                100_000_031,
                35832,
                100_000_035,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_032,
                56201,
                100_000_035,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_033,
                4247,
                100_000_035,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_034,
                47763,
                100_000_035,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_040,
                21924,
                100_000_001,
                InventoryFlag::HiSlot0,
                2_000,
                false,
            ),
            (
                100_000_041,
                21924,
                100_000_029,
                InventoryFlag::HiSlot0,
                2_000,
                false,
            ),
        ];
        for (item_id, type_id, location_id, flag, quantity, singleton) in extra_items {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: 90_000_001,
                    location_id,
                    flag,
                    quantity,
                    singleton,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        snap.items.insert(
            100_000_042,
            InventoryItemRecord {
                item_id: 100_000_042,
                type_id: 21924,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::HiSlot0,
                quantity: 2_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        let char_skills = snap.skills.entry(90_000_001).or_default();
        for (skill_id, level, sp) in [
            (3388, 4, 45_255.0),
            (11445, 4, 226_275.0),
            (11448, 4, 226_275.0),
            (11452, 4, 226_275.0),
            (21791, 4, 226_275.0),
            (24242, 4, 45_255.0),
            (33399, 4, 90_510.0),
            (33407, 4, 90_510.0),
            (21611, 2, 12_699.0),
        ] {
            char_skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: sp,
                },
            );
        }
    });
}

pub(crate) async fn login_and_select_aura(ctx: &ServerContext, session: &mut SessionState) {
    seed_test_expansion_assets(ctx);
    ctx.dispatch_rpc(
        session,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();

    ctx.dispatch_rpc(
        session,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
}
