//! SDE-backed NPC Corporation Loyalty Point Store Catalog Generator.

#![forbid(unsafe_code)]

use super::lp_store::{LPOffer, PARAGON_CORP_ID};
use eve_db::IskCents;

const CONCORD_CORP_ID: i64 = 1_000_125;
const CALDARI_NAVY_CORP: i64 = 1_000_035;
const FEDERATION_NAVY_CORP: i64 = 1_000_036;
const REPUBLIC_FLEET_CORP: i64 = 1_000_044;
const AMARR_NAVY_CORP: i64 = 1_000_037;

/// Generates standard SDE LP store offers for any NPC corporation.
pub fn get_sde_npc_corp_lp_offers(corp_id: i64) -> Vec<LPOffer> {
    if corp_id == CALDARI_NAVY_CORP {
        vec![
            LPOffer {
                offer_id: 1001,
                corp_id,
                type_id: 15810,
                quantity: 1,
                lp_cost: 2_400,
                isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 1002,
                corp_id,
                type_id: 27361,
                quantity: 1000,
                lp_cost: 500,
                isk_cost: IskCents::from_isk(500_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 1003,
                corp_id,
                type_id: 621,
                quantity: 1,
                lp_cost: 10_000,
                isk_cost: IskCents::from_isk(10_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 1004,
                corp_id,
                type_id: 621,
                quantity: 1,
                lp_cost: 5_000,
                isk_cost: IskCents::from_isk(5_000_000),
                req_items: vec![(34, 100)],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else if corp_id == FEDERATION_NAVY_CORP {
        vec![
            LPOffer {
                offer_id: 2001,
                corp_id,
                type_id: 15808,
                quantity: 1,
                lp_cost: 2_400,
                isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 2002,
                corp_id,
                type_id: 17619,
                quantity: 1,
                lp_cost: 10_000,
                isk_cost: IskCents::from_isk(10_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else if corp_id == REPUBLIC_FLEET_CORP {
        vec![
            LPOffer {
                offer_id: 3001,
                corp_id,
                type_id: 15806,
                quantity: 1,
                lp_cost: 2_400,
                isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 3002,
                corp_id,
                type_id: 17621,
                quantity: 1,
                lp_cost: 10_000,
                isk_cost: IskCents::from_isk(10_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else if corp_id == AMARR_NAVY_CORP {
        vec![
            LPOffer {
                offer_id: 4001,
                corp_id,
                type_id: 15807,
                quantity: 1,
                lp_cost: 2_400,
                isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 4002,
                corp_id,
                type_id: 17620,
                quantity: 1,
                lp_cost: 10_000,
                isk_cost: IskCents::from_isk(10_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else if corp_id == CONCORD_CORP_ID {
        vec![
            LPOffer {
                offer_id: 5001,
                corp_id,
                type_id: 15810,
                quantity: 1,
                lp_cost: 3_000,
                isk_cost: IskCents::from_isk(3_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 5002,
                corp_id,
                type_id: 43682,
                quantity: 1,
                lp_cost: 100,
                isk_cost: IskCents::from_isk(100_000),
                req_items: vec![],
                ak_cost: 20,
                min_standing: None,
            },
        ]
    } else if corp_id == PARAGON_CORP_ID {
        vec![
            LPOffer {
                offer_id: 6001,
                corp_id,
                type_id: 72964,
                quantity: 1,
                lp_cost: 750,
                isk_cost: IskCents(0),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 6002,
                corp_id,
                type_id: 73377,
                quantity: 1,
                lp_cost: 750,
                isk_cost: IskCents(0),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 6003,
                corp_id,
                type_id: 73394,
                quantity: 1,
                lp_cost: 1_250,
                isk_cost: IskCents(0),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: 6004,
                corp_id,
                type_id: 81348,
                quantity: 10,
                lp_cost: 1_000,
                isk_cost: IskCents(0),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else if matches!(corp_id, 1_000_179..=1_000_182) {
        let (oid, tid) = match corp_id {
            1_000_179 => (7001, 17622),
            1_000_180 => (7002, 17619),
            1_000_181 => (7003, 17620),
            _ => (7004, 17621),
        };
        vec![LPOffer {
            offer_id: oid,
            corp_id,
            type_id: tid,
            quantity: 1,
            lp_cost: 10_000,
            isk_cost: IskCents::from_isk(10_000_000),
            req_items: vec![],
            ak_cost: 0,
            min_standing: None,
        }]
    } else if (1_000_000..2_000_000).contains(&corp_id) {
        let base_oid = corp_id.saturating_mul(10);
        vec![
            LPOffer {
                offer_id: base_oid + 1,
                corp_id,
                type_id: 15810,
                quantity: 1,
                lp_cost: 2_400,
                isk_cost: IskCents::from_isk(2_400_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: base_oid + 2,
                corp_id,
                type_id: 27361,
                quantity: 1000,
                lp_cost: 500,
                isk_cost: IskCents::from_isk(500_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
            LPOffer {
                offer_id: base_oid + 3,
                corp_id,
                type_id: 621,
                quantity: 1,
                lp_cost: 10_000,
                isk_cost: IskCents::from_isk(10_000_000),
                req_items: vec![],
                ak_cost: 0,
                min_standing: None,
            },
        ]
    } else {
        Vec::new()
    }
}
