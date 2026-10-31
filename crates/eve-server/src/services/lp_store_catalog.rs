//! `lp_store_catalog` — Static & Dynamic Loyalty Point (LP) Store Offer Catalogs.
//!
//! Resolves NPC corporation LP store catalogs in priority order:
//! 1. Dynamic runtime overrides persisted in `WorldStateSnapshot` (`service_state_blobs["lp_store_offers"]`
//!    or `loyalty_store_offers`);
//! 2. Paragon (`1_000_419`) Ship Cosmetic Emblem EverMark offers from V24
//!    `res:/staticdata/shipcosmeticlicenses.fsdbinary`;
//! 3. Authoritative CCP Tranquility ESI LP store catalogs imported into `data/esi_lp_store_offers.json`
//!    for the 181 NPC corporations with `lp_offer_tables_json` in the V24 SDE.

#![forbid(unsafe_code)]

use eve_db::IskCents;
use eve_proto::PyRep;
use eve_sde::TypeId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

pub const CONCORD_CORP_ID: i64 = 1_000_125;
pub const PARAGON_CORP_ID: i64 = 1_000_419;
pub const LP_STORE_OFFERS_KEY: &str = "lp_store_offers";

const RAW_ESI_LP_OFFERS_JSON: &str = include_str!("../../../../data/esi_lp_store_offers.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LPOffer {
    pub offer_id: i64,
    pub corp_id: i64,
    pub type_id: TypeId,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: IskCents,
    pub req_items: Vec<(TypeId, i32)>,
    #[serde(default)]
    pub ak_cost: i64,
    #[serde(default)]
    pub min_standing: Option<f64>,
}

#[derive(Deserialize)]
struct RawEsiLpCatalog {
    offers: HashMap<String, (u32, i32, i64, i64, i64, Vec<(u32, i32)>)>,
    corp_offers: HashMap<String, Vec<i64>>,
}

/// Parses `data/esi_lp_store_offers.json` once into an immutable `corp_id -> Vec<LPOffer>` table.
fn esi_lp_catalog() -> &'static HashMap<i64, Vec<LPOffer>> {
    static CATALOG: OnceLock<HashMap<i64, Vec<LPOffer>>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let Ok(raw) = serde_json::from_str::<RawEsiLpCatalog>(RAW_ESI_LP_OFFERS_JSON) else {
            return HashMap::new();
        };
        let mut parsed_offers: HashMap<i64, (u32, i32, i64, i64, i64, Vec<(u32, i32)>)> =
            HashMap::with_capacity(raw.offers.len());
        for (oid_str, tuple) in raw.offers {
            if let Ok(oid) = oid_str.parse::<i64>() {
                parsed_offers.insert(oid, tuple);
            }
        }
        let mut by_corp: HashMap<i64, Vec<LPOffer>> =
            HashMap::with_capacity(raw.corp_offers.len());
        for (cid_str, oids) in raw.corp_offers {
            let Ok(corp_id) = cid_str.parse::<i64>() else {
                continue;
            };
            let mut list = Vec::with_capacity(oids.len());
            for oid in oids {
                let Some((type_id, quantity, lp_cost, isk_cost, ak_cost, req_items)) =
                    parsed_offers.get(&oid)
                else {
                    continue;
                };
                list.push(LPOffer {
                    offer_id: oid,
                    corp_id,
                    type_id: *type_id,
                    quantity: *quantity,
                    lp_cost: *lp_cost,
                    isk_cost: IskCents::from_isk(*isk_cost),
                    req_items: req_items.clone(),
                    ak_cost: *ak_cost,
                    min_standing: None,
                });
            }
            list.sort_by_key(|o| o.offer_id);
            if !list.is_empty() {
                by_corp.insert(corp_id, list);
            }
        }
        by_corp
    })
}

/// Returns authoritative Tranquility ESI LP store offers for `corp_id`, filtered against V24 `sde.types`.
pub fn esi_sde_lp_offers(sde: &eve_sde::SdeRegistry, corp_id: i64) -> Vec<LPOffer> {
    let Some(offers) = esi_lp_catalog().get(&corp_id) else {
        return Vec::new();
    };
    if sde.types.is_empty() {
        return offers.clone();
    }
    offers
        .iter()
        .filter(|o| {
            sde.types.contains_key(&o.type_id)
                && o.req_items.iter().all(|(rt, _)| sde.types.contains_key(rt))
        })
        .cloned()
        .collect()
}

/// Returns all imported Tranquility ESI LP offers across corporations when resolving 2-arg `TakeOffer*` calls.
pub fn esi_all_lp_offers(sde: &eve_sde::SdeRegistry) -> Vec<LPOffer> {
    let mut corp_ids: Vec<i64> = esi_lp_catalog().keys().copied().collect();
    corp_ids.sort_unstable();
    let mut out = Vec::new();
    for cid in corp_ids {
        out.extend(esi_sde_lp_offers(sde, cid));
    }
    out
}

/// Load persisted dynamic LP store offers from `WorldStateSnapshot.service_state_blobs["lp_store_offers"]`.
pub fn load_lp_store_offers(snap: &eve_db::WorldStateSnapshot) -> Vec<LPOffer> {
    snap.service_state_blobs
        .get(LP_STORE_OFFERS_KEY)
        .and_then(|json| serde_json::from_str::<Vec<LPOffer>>(json).ok())
        .unwrap_or_default()
}

/// DERIVED: Synthesized EverMark price tiers by hull group and cosmetic type (`1` = corp, `2` = alliance).
/// `res:/staticdata/shipcosmeticlicenses.fsdbinary` defines the published emblem offer list, while
/// EverMark prices are server-authored (`DERIVED`).
fn paragon_emblem_evermark_cost(ship_group_id: u32, cosmetic_type: u8) -> i64 {
    let base = match ship_group_id {
        25 | 31 | 237 => 750,
        324 | 420 | 541 | 830 | 831 | 834 | 893 | 1305 | 1527 | 1534 => 1_500,
        26 | 28 | 463 => 2_500,
        358 | 380 | 419 | 540 | 543 | 832 | 833 | 894 | 906 | 963 | 1201 | 1202 | 1972 => 4_000,
        27 | 513 | 941 => 7_500,
        485 | 547 | 659 | 883 | 898 | 900 | 902 | 1538 | 4594 | 30 => 15_000,
        _ => 2_500,
    };
    if cosmetic_type == 2 { (base * 5) / 3 } else { base }
}

/// Loads Paragon (`1_000_419`) Ship Cosmetic Emblem offers from
/// `res:/staticdata/shipcosmeticlicenses.fsdbinary` (offer list from V24 SDE; EverMark prices `DERIVED`).
pub fn paragon_sde_evermark_offers(sde: &eve_sde::SdeRegistry) -> &'static [LPOffer] {
    static PARAGON_OFFERS: OnceLock<Vec<LPOffer>> = OnceLock::new();
    PARAGON_OFFERS.get_or_init(|| {
        let mut offers = Vec::new();
        let Ok(rows) = sde.load_cfsd_table("res:/staticdata/shipcosmeticlicenses.fsdbinary") else {
            return offers;
        };
        let get_u32 = |rec: &eve_sde::CfsdRecord, k: &str| match rec.fields.get(k) {
            Some(eve_sde::CfsdValue::Int(i)) => u32::try_from(*i).ok(),
            _ => None,
        };
        for rec in rows.values() {
            let (Some(license_type_id), Some(ship_type_id), Some(ctype)) = (
                get_u32(rec, "shipCosmeticLicenseTypeID"),
                get_u32(rec, "shipTypeID"),
                get_u32(rec, "cosmeticType"),
            ) else {
                continue;
            };
            if !sde.types.get(&license_type_id).is_some_and(|t| t.published) {
                continue;
            }
            let ship_group_id = sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(25);
            offers.push(LPOffer {
                offer_id: 600_000 + i64::from(license_type_id),
                corp_id: PARAGON_CORP_ID,
                type_id: license_type_id,
                quantity: 1,
                lp_cost: paragon_emblem_evermark_cost(ship_group_id, ctype as u8),
                isk_cost: IskCents::ZERO,
                req_items: Vec::new(),
                ak_cost: 0,
                min_standing: None,
            });
        }
        offers.sort_by_key(|o| o.offer_id);
        offers
    })
}

/// Load all available LP store offers for a corporation:
/// dynamic `WorldStateSnapshot` overrides first, then V24 SDE Paragon EverMarks offers for `1_000_419`,
/// otherwise authoritative Tranquility ESI LP store offers for `corp_id`.
pub fn get_corp_lp_offers_with_sde(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
) -> Vec<LPOffer> {
    let offers = get_corp_lp_offers(snap, corp_id);
    if !offers.is_empty() {
        return offers;
    }
    if corp_id == PARAGON_CORP_ID {
        return paragon_sde_evermark_offers(sde).to_vec();
    }
    esi_sde_lp_offers(sde, corp_id)
}

/// Load dynamic LP store offer overrides for a corporation from `WorldStateSnapshot`.
pub fn get_corp_lp_offers(snap: &eve_db::WorldStateSnapshot, corp_id: i64) -> Vec<LPOffer> {
    let mut offers: Vec<_> = load_lp_store_offers(snap)
        .into_iter()
        .filter(|o| o.corp_id == corp_id)
        .collect();
    if offers.is_empty() {
        if let Some(legacy) = snap.loyalty_store_offers.get(&corp_id) {
            for o in legacy {
                offers.push(LPOffer {
                    offer_id: o.offer_id,
                    corp_id,
                    type_id: o.type_id as TypeId,
                    quantity: o.quantity as i32,
                    lp_cost: o.lp_cost,
                    isk_cost: IskCents(o.isk_cost * 100),
                    ak_cost: o.ak_cost.unwrap_or(0),
                    req_items: o
                        .required_items
                        .iter()
                        .map(|ri| (ri.type_id as TypeId, ri.quantity as i32))
                        .collect(),
                    min_standing: None,
                });
            }
        }
    }
    offers
}

pub fn save_lp_store_offers(snap: &mut eve_db::WorldStateSnapshot, offers: &[LPOffer]) {
    if let Ok(json) = serde_json::to_string(offers) {
        snap.service_state_blobs
            .insert(LP_STORE_OFFERS_KEY.to_string(), json);
    }
}

pub fn register_lp_store_offer(snap: &mut eve_db::WorldStateSnapshot, offer: LPOffer) {
    let mut offers = load_lp_store_offers(snap);
    if let Some(existing) = offers
        .iter_mut()
        .find(|o| o.corp_id == offer.corp_id && o.offer_id == offer.offer_id)
    {
        *existing = offer;
    } else {
        offers.push(offer);
    }
    save_lp_store_offers(snap, &offers);
}

pub fn load_corp_lp_balances(snap: &eve_db::WorldStateSnapshot, corp_id: i64) -> HashMap<i64, i64> {
    snap.corp_lp_balances
        .iter()
        .filter_map(|(&(cid, issuer_id), &lp)| (cid == corp_id).then_some((issuer_id, lp)))
        .collect()
}

pub fn save_corp_lp_balances(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    balances: &HashMap<i64, i64>,
) {
    snap.corp_lp_balances.retain(|&(cid, _), _| cid != corp_id);
    for (&issuer_id, &lp) in balances {
        snap.corp_lp_balances.insert((corp_id, issuer_id), lp);
    }
}

pub(crate) fn offer_to_pyrep(o: &LPOffer) -> PyRep {
    let req_items_rep: Vec<PyRep> = o
        .req_items
        .iter()
        .map(|&(tid, qty)| {
            PyRep::keyval([
                ("typeID", PyRep::Integer(tid as i64)),
                ("quantity", PyRep::Integer(qty as i64)),
            ])
        })
        .collect();
    let mut fields = vec![
        ("offerID", PyRep::Integer(o.offer_id)),
        ("corpID", PyRep::Integer(o.corp_id)),
        ("typeID", PyRep::Integer(o.type_id as i64)),
        ("quantity", PyRep::Integer(o.quantity as i64)),
        ("lpCost", PyRep::Integer(o.lp_cost)),
        ("iskCost", PyRep::Real(o.isk_cost.to_wire_f64())),
        ("akCost", PyRep::Integer(o.ak_cost.max(0))),
        ("reqItems", PyRep::List(req_items_rep)),
    ];
    if let Some(min_st) = o.min_standing {
        fields.push(("minStanding", PyRep::Real(min_st)));
    }
    PyRep::keyval(fields)
}
