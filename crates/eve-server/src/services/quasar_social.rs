//! `quasar_social` — Quasar Social & Organization Gateway Handler Registration (`Q-5`).

use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::WorldRepository;
use std::sync::Arc;

pub fn register_social_services(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    crate::services::quasar_social_chat::register(router, db.clone());
    crate::services::quasar_social_corp::register(router, db.clone());
    crate::services::quasar_social_freelance::register(router, db.clone());
    crate::services::quasar_social_community::register(router, db);
}
