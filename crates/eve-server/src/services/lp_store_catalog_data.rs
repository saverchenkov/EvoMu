//! Static NPC Corporation Loyalty Point (LP) Store Catalog.
//!
//! # Provenance & Data Coverage
//! - Imported from CCP Crucible SQL dump (`/Users/sergeya/ai/evemu_Crucible/sql/base/lpStore.sql.gz`,
//!   `29,070` offers across `163` NPC corporations, and `lpRequiredItems.sql.gz`, `33,470` raw rows /
//!   `33,342` trade-in rows matching `lpStore.sql.gz`) into `lp_store_catalog.zlib`.
//! - Covers major empire navies, CONCORD (`1000125`, `215` offers), Caldari Navy (`1000035`, `325`
//!   offers), Federation Navy (`1000120`), Imperial Navy (`1000084`), Republic Fleet (`1000051`),
//!   pirate factions, R&D corporations, and empire schools/ministries.
//! - DATA GAP: Post-Crucible NPC LP stores (such as 24th Imperial Crusade / EverMark / Zarzakh
//!   stores added after the Crucible era) are server-only in modern EVE and absent from both V24
//!   client FSD (`SharedCache`) and the Crucible SQL dump. Those stores can be populated at runtime
//!   via `WorldStateSnapshot.service_state_blobs["lp_store_offers"]` (`register_lp_store_offer` /
//!   `save_lp_store_offers`) or `snap.loyalty_store_offers`, which merge with and override this
//!   static catalog by `(corp_id, offer_id)`.

#![forbid(unsafe_code)]

use crate::services::lp_store::LPOffer;
use eve_db::IskCents;
use eve_sde::TypeId;
use std::collections::HashMap;
use std::sync::OnceLock;

const CATALOG_MAGIC: &[u8; 8] = b"EVOLP001";
const COMPRESSED_CATALOG: &[u8] = include_bytes!("lp_store_catalog.zlib");

struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        if end > self.buf.len() {
            return None;
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn read_u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i32(&mut self) -> Option<i32> {
        let b = self.take(4)?;
        Some(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i64(&mut self) -> Option<i64> {
        let b = self.take(8)?;
        Some(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

fn decode_catalog() -> HashMap<i64, Vec<LPOffer>> {
    let Ok(raw) = miniz_oxide::inflate::decompress_to_vec_zlib(COMPRESSED_CATALOG) else {
        return HashMap::new();
    };
    let mut reader = ByteReader::new(&raw);
    let Some(magic) = reader.take(8) else {
        return HashMap::new();
    };
    if magic != CATALOG_MAGIC {
        return HashMap::new();
    }
    let Some(corp_count) = reader.read_u32() else {
        return HashMap::new();
    };
    let mut map = HashMap::with_capacity(corp_count as usize);
    for _ in 0..corp_count {
        let (Some(corp_id), Some(offer_count)) = (reader.read_i64(), reader.read_u32()) else {
            break;
        };
        let mut offers = Vec::with_capacity(offer_count as usize);
        for _ in 0..offer_count {
            let (
                Some(offer_id),
                Some(type_id),
                Some(quantity),
                Some(lp_cost),
                Some(isk_cents),
                Some(req_count),
            ) = (
                reader.read_i64(),
                reader.read_u32(),
                reader.read_i32(),
                reader.read_i64(),
                reader.read_i64(),
                reader.read_u16(),
            )
            else {
                break;
            };
            let mut req_items = Vec::with_capacity(req_count as usize);
            for _ in 0..req_count {
                if let (Some(req_type_id), Some(req_qty)) = (reader.read_u32(), reader.read_i32()) {
                    req_items.push((req_type_id as TypeId, req_qty));
                }
            }
            offers.push(LPOffer {
                offer_id,
                corp_id,
                type_id: type_id as TypeId,
                quantity,
                lp_cost,
                isk_cost: IskCents(isk_cents),
                req_items,
                ak_cost: 0,
                min_standing: None,
            });
        }
        map.insert(corp_id, offers);
    }
    map
}

fn catalog() -> &'static HashMap<i64, Vec<LPOffer>> {
    static CATALOG: OnceLock<HashMap<i64, Vec<LPOffer>>> = OnceLock::new();
    CATALOG.get_or_init(decode_catalog)
}

/// Returns the static CCP Crucible LP store offers for `corp_id`, sorted by `offer_id`.
pub fn static_corp_lp_offers(corp_id: i64) -> &'static [LPOffer] {
    catalog().get(&corp_id).map(Vec::as_slice).unwrap_or(&[])
}

/// Returns the total number of static LP store offers in the imported catalog (`29,070`).
pub fn total_static_offer_count() -> usize {
    catalog().values().map(Vec::len).sum()
}

/// Returns the number of NPC corporations with static LP store offers (`163`).
pub fn static_corp_count() -> usize {
    catalog().len()
}
