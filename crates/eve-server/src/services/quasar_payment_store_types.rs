//! Shared types and default data for Quasar Payment, PLEX Vault, and NES Store.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlexTransactionRecord {
    pub id: u64,
    pub user_id: u64,
    pub amount: i64,
    pub balance_after: i64,
    pub timestamp_unix: u64,
    pub description: String,
}

pub fn get_plex_transactions(snap: &eve_db::WorldStateSnapshot) -> Vec<PlexTransactionRecord> {
    snap.service_state_blobs
        .get("plex_tx_logs")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

pub fn record_plex_transaction(
    snap: &mut eve_db::WorldStateSnapshot,
    tx_id: u64,
    user_id: u64,
    amount: i64,
    balance_after: i64,
    description: &str,
    now_unix: u64,
) {
    let mut logs = get_plex_transactions(snap);
    logs.push(PlexTransactionRecord {
        id: tx_id,
        user_id,
        amount,
        balance_after,
        timestamp_unix: now_unix,
        description: description.to_string(),
    });
    if let Ok(serialized) = serde_json::to_string(&logs) {
        snap.service_state_blobs.insert("plex_tx_logs".to_string(), serialized);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCategory {
    pub id: u32,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreProduct {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub type_id: u32,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreOffer {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub price_plex: i64,
    pub product_ids: Vec<u32>,
    pub targeted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCatalog {
    pub categories: Vec<StoreCategory>,
    pub products: Vec<StoreProduct>,
    pub offers: Vec<StoreOffer>,
}

// DERIVED: NES store catalog is server-authored (delivered to client via storeManager and eve_public.store.*); seeded from published SDE types: 34599 (Apocalypse Kador SKIN, category 91), 40519 (Skill Extractor, category 5), 3966 (Men's Precision Boots, category 30 Apparel).
pub fn get_default_store_catalog() -> StoreCatalog {
    StoreCatalog {
        categories: vec![
            StoreCategory {
                id: 1,
                name: "Clone State & Omega".to_string(),
                description: "Omega Clone State subscriptions".to_string(),
            },
            StoreCategory {
                id: 2,
                name: "Ship SKINs".to_string(),
                description: "Permanent Ship Cosmetic Licenses".to_string(),
            },
            StoreCategory {
                id: 3,
                name: "Services & Pilot Training".to_string(),
                description: "Pilot services and skill extraction".to_string(),
            },
            StoreCategory {
                id: 4,
                name: "Apparel & Collectibles".to_string(),
                description: "Pilot customizations and vanity items".to_string(),
            },
        ],
        products: vec![
            StoreProduct {
                id: 101,
                name: "30 Days Omega Clone State".to_string(),
                description: "30 Days Omega clone subscription".to_string(),
                type_id: 0,
                quantity: 1,
            },
            StoreProduct {
                id: 102,
                name: "Ship SKIN Offer".to_string(),
                description: "Ship cosmetic license".to_string(),
                type_id: 34599, // Apocalypse Kador SKIN (category 91)
                quantity: 1,
            },
            StoreProduct {
                id: 103,
                name: "Skill Extractor".to_string(),
                description: "Skill point extraction tool".to_string(),
                type_id: 40519,
                quantity: 1,
            },
            StoreProduct {
                id: 104,
                name: "Pilot Apparel Offer".to_string(),
                description: "Men's Precision Boots apparel".to_string(),
                type_id: 3966, // Men's Precision Boots (category 30)
                quantity: 1,
            },
        ],
        offers: vec![
            StoreOffer {
                id: 1001,
                name: "30 Days Omega Clone State".to_string(),
                description: "30 Days Omega clone subscription".to_string(),
                category_id: 1,
                price_plex: 500,
                product_ids: vec![101],
                targeted: false,
            },
            StoreOffer {
                id: 1002,
                name: "Ship SKIN Offer".to_string(),
                description: "Ship cosmetic license".to_string(),
                category_id: 2,
                price_plex: 110,
                product_ids: vec![102],
                targeted: false,
            },
            StoreOffer {
                id: 1003,
                name: "Skill Extractor".to_string(),
                description: "Skill point extraction tool".to_string(),
                category_id: 3,
                price_plex: 140,
                product_ids: vec![103],
                targeted: false,
            },
            StoreOffer {
                id: 1004,
                name: "Pilot Apparel Offer".to_string(),
                description: "Men's Precision Boots apparel".to_string(),
                category_id: 4,
                price_plex: 75,
                product_ids: vec![104],
                targeted: false,
            },
            StoreOffer {
                id: 2001,
                name: "Targeted Omega Bundle".to_string(),
                description: "Special targeted clone subscription".to_string(),
                category_id: 1,
                price_plex: 450,
                product_ids: vec![101],
                targeted: true,
            },
            StoreOffer {
                id: 3001,
                name: "Personalized Starter Offer".to_string(),
                description: "Personalized skill extraction offer".to_string(),
                category_id: 3,
                price_plex: 120,
                product_ids: vec![103],
                targeted: true,
            },
        ],
    }
}

pub fn save_store_catalog(snap: &mut eve_db::WorldStateSnapshot, catalog: &StoreCatalog) {
    if let Ok(serialized) = serde_json::to_string(catalog) {
        snap.service_state_blobs.insert("nes_store_catalog".to_string(), serialized);
    }
}

pub fn read_store_catalog(snap: &eve_db::WorldStateSnapshot) -> StoreCatalog {
    snap.service_state_blobs
        .get("nes_store_catalog")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_else(get_default_store_catalog)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentScheduleRecord {
    pub id: u64,
    pub user_id: u64,
    pub schedule_type: u32,
    pub next_payment_unix: u64,
    pub offer_id: String,
    pub interval_days: u32,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentTokenRecord {
    pub id: u32,
    pub user_id: u64,
    pub token_str: String,
    pub is_quick_pay: bool,
    #[serde(default)]
    pub consumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentReceiptRecord {
    pub receipt_id: u64,
    pub user_id: u64,
    pub offer_id: String,
    pub amount_in_cents: u32,
    pub currency: String,
    pub timestamp_unix: u64,
}
