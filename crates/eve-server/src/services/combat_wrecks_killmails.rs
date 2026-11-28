//! `combat_wrecks_killmails` — Wreck/Jetcan Lifetimes, LootAll, Detailed Killmails, SHA-1 ESI Hash, Crystal Damage & Self-Destruct (`C-1`, `C-3`).

#![forbid(unsafe_code)]

use super::*;

#[path = "combat_wrecks_loot_ops.rs"]
mod combat_wrecks_loot_ops;

pub(crate) use combat_wrecks_loot_ops::authorize_killmail_read;

pub const INSURANCE_CONTRACT_DURATION_SECS: u64 = 84 * 86_400; // 12 weeks (84 days)

/// Computes the 6-level insurance quote table (`0.5` Basic .. `1.0` Platinum) for a ship type (`feat:combat.insurance-quote`).
pub fn compute_ship_insurance_quote_levels(
    ctx: &ServerContext,
    ship_type_id: u32,
) -> (f64, Vec<(f64, &'static str, f64, f64)>) {
    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, ship_type_id);
    let fracs = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
    let levels = compute_insurance_levels(ref_val).into_iter().enumerate().map(|(idx, (name, cost, payout))| (fracs[idx], name, cost, payout)).collect();
    (ref_val, levels)
}

/// Synchronizes `snap.insurance_contracts` with `snap.items`, creating 12-week contract
/// records for insured ships lacking a contract row and voiding contracts whose ship expired,
/// was repackaged (`!singleton`), destroyed, or transferred/sold
/// (`feat:ships.ship-insurance-link`, `feat:combat.insurance-contracts`, `#736`, `#1238` EP-13).
pub fn sync_ship_insurance_contracts(ctx: &ServerContext, owner_id: i64) {
    let now_unix = ctx.now_unix();
    let (voided_ship_ids, new_contracts): (
        Vec<i64>,
        Vec<(i64, eve_db::PersistedInsuranceContractRecord)>,
    ) = ctx.db.with_snapshot(|snap| {
        let mut ship_by_id: HashMap<i64, (u32, i64, bool, f64)> = HashMap::new();
        for it in snap.items.values() {
            if ctx
                .sde
                .types
                .get(&it.type_id)
                .is_some_and(|t| t.category_id == 6)
            {
                ship_by_id.insert(
                    it.item_id,
                    (it.type_id, it.owner_id, it.singleton, it.insured_payout_isk),
                );
            }
        }
        let mut voided = Vec::new();
        for (&sid, c) in snap.insurance_contracts.iter() {
            if c.end_unix <= now_unix {
                voided.push(sid);
                continue;
            }
            if let Some(&(_, cur_owner, singleton, payout)) = ship_by_id.get(&sid) {
                if !singleton || cur_owner != c.owner_id || payout <= 0.0 {
                    voided.push(sid);
                }
            } else {
                voided.push(sid);
            }
        }
        let mut inserted = Vec::new();
        for (&sid, &(type_id, item_owner, singleton, payout_isk)) in &ship_by_id {
            if singleton
                && payout_isk > 0.0
                && (item_owner == owner_id || owner_id == 0)
                && !snap.insurance_contracts.contains_key(&sid)
                && !voided.contains(&sid)
            {
                let ref_val = compute_ship_insurance_reference_value(&ctx.sde, type_id);
                let frac = (payout_isk / ref_val.max(1.0)).clamp(0.4, 1.0);
                let (cost_isk, _) = compute_insurance_quote(ref_val, frac);
                let level_name = if frac >= 0.99 {
                    "Platinum".to_string()
                } else {
                    "Standard".to_string()
                };
                inserted.push((
                    sid,
                    eve_db::PersistedInsuranceContractRecord {
                        ship_id: sid,
                        ship_type_id: type_id,
                        owner_id: item_owner,
                        level_name,
                        level_fraction: frac,
                        cost_isk,
                        payout_isk,
                        start_unix: now_unix,
                        end_unix: now_unix.saturating_add(INSURANCE_CONTRACT_DURATION_SECS),
                    },
                ));
            }
        }
        (voided, inserted)
    });
    if voided_ship_ids.is_empty() && new_contracts.is_empty() {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        for sid in voided_ship_ids {
            snap.insurance_contracts.remove(&sid);
            if let Some(it) = snap.items.get_mut(&sid) {
                if it.insured_payout_isk > 0.0 {
                    it.insured_payout_isk = 0.0;
                }
            }
        }
        for (sid, rec) in new_contracts {
            snap.insurance_contracts.insert(sid, rec);
        }
    });
}

/// Sweeps expired ship insurance contracts on periodic background cluster ticks (`#736`).
pub fn sweep_expired_insurance_contracts(ctx: &ServerContext, now_unix: u64) {
    let expired_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.insurance_contracts
            .iter()
            .filter(|(_, c)| c.end_unix <= now_unix)
            .map(|(&sid, _)| sid)
            .collect()
    });
    if expired_ids.is_empty() {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &sid in &expired_ids {
            snap.insurance_contracts.remove(&sid);
            if let Some(it) = snap.items.get_mut(&sid) {
                if it.insured_payout_isk > 0.0 {
                    it.insured_payout_isk = 0.0;
                }
            }
        }
    });
}

/// Voids an active ship insurance contract when the ship is repackaged, traded, or un-insured (`feat:ships.ship-insurance-link`, `feat:combat.insurance-contracts`).
pub fn void_ship_insurance_contract(ctx: &ServerContext, ship_id: i64) -> bool {
    ctx.db.with_mut_snapshot(|snap| {
        let removed_persisted = snap.insurance_contracts.remove(&ship_id).is_some();
        let removed_item = if let Some(it) = snap.items.get_mut(&ship_id) {
            let had = it.insured_payout_isk > 0.0;
            if had {
                it.insured_payout_isk = 0.0;
            }
            had
        } else {
            false
        };
        removed_persisted || removed_item
    })
}

/// Outcome of atomically settling a ship's insurance payout inside a DB transaction (`#1238` EP-13).
#[derive(Debug, Default)]
pub(crate) struct ShipInsuranceSettlement {
    pub payout_isk: f64,
    pub is_corp: bool,
    pub credited_owner_id: i64,
    pub balance_after_isk: f64,
    pub mail_id: Option<i64>,
    pub mail_subject: String,
}

/// Atomically resolves, idempotency-checks, credits, journals, and mails ship insurance inside
/// an open DB snapshot transaction (`#1238` EP-13).
pub(crate) fn settle_ship_insurance_payout_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    victim_char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    no_payout: bool,
    mark_destroyed_marker: bool,
    now_unix: u64,
) -> ShipInsuranceSettlement {
    let idempotency_key = (ship_id, "ship_insurance_payout".to_string());
    let already_paid = snap.committed_idempotency_keys.contains(&idempotency_key)
        || snap
            .items
            .get(&ship_id)
            .is_some_and(|it| it.insured_payout_isk < 0.0);
    let contract_opt = snap.insurance_contracts.remove(&ship_id);
    if already_paid {
        return ShipInsuranceSettlement::default();
    }
    if no_payout || ship_type_id == 670 {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.insured_payout_isk = if mark_destroyed_marker { -1.0 } else { 0.0 };
        }
        if mark_destroyed_marker {
            snap.committed_idempotency_keys.insert(idempotency_key);
        }
        return ShipInsuranceSettlement::default();
    }
    let item_insured_payout = snap
        .items
        .get(&ship_id)
        .map(|it| it.insured_payout_isk.max(0.0))
        .unwrap_or(0.0);
    let (payout_isk, contract_owner_id) = match contract_opt {
        Some(ref c) if c.end_unix > now_unix && c.payout_isk > 0.0 => {
            (c.payout_isk, (c.owner_id > 0).then_some(c.owner_id))
        }
        Some(_) => {
            let ref_val = compute_ship_insurance_reference_value(sde, ship_type_id);
            (compute_insurance_quote(ref_val, 0.40).1, None)
        }
        None if item_insured_payout > 0.0 => (item_insured_payout, None),
        None => {
            let ref_val = compute_ship_insurance_reference_value(sde, ship_type_id);
            (compute_insurance_quote(ref_val, 0.40).1, None)
        }
    };
    if payout_isk <= 0.0 {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.insured_payout_isk = if mark_destroyed_marker { -1.0 } else { 0.0 };
        }
        if mark_destroyed_marker {
            snap.committed_idempotency_keys.insert(idempotency_key);
        }
        return ShipInsuranceSettlement::default();
    }
    let payout_cents = IskCents::from_f64_isk(payout_isk);
    let mut is_corp = false;
    let mut bal_after = None;
    let mut journal_target = None;
    let allow_victim_fallback = contract_owner_id.is_none() || contract_owner_id == Some(victim_char_id);
    if let Some(owner_id) = contract_owner_id {
        if let Some(corp) = snap.corporations.get_mut(&owner_id) {
            bal_after = Some(corp.credit_division(1, payout_cents));
            journal_target = Some(owner_id);
            is_corp = true;
        }
    }
    if journal_target.is_none() && allow_victim_fallback {
        if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
            ch.balance = ch.balance.checked_add(payout_cents).unwrap_or(ch.balance);
            bal_after = Some(ch.balance);
            journal_target = Some(victim_char_id);
        }
    }
    let (Some(ba), Some(jid_target)) = (bal_after, journal_target) else {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.insured_payout_isk = if mark_destroyed_marker { -1.0 } else { 0.0 };
        }
        if mark_destroyed_marker {
            snap.committed_idempotency_keys.insert(idempotency_key);
        }
        return ShipInsuranceSettlement::default();
    };
    if let Some(it) = snap.items.get_mut(&ship_id) {
        it.insured_payout_isk = -1.0;
    }
    snap.committed_idempotency_keys.insert(idempotency_key);
    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
        owner_id: jid_target,
        entry_type: "insurance_payout",
        amount: payout_cents,
        balance_after: ba,
        reason: format!("Insurance payout for loss of ship {ship_id}"),
        meta: eve_db::ledger::JournalMeta::new(
            eve_db::ledger::ref_types::INSURANCE,
            1_000_132,
            jid_target,
            now_unix,
        )
        .with_context(eve_db::ledger::JournalContext::Type, i64::from(ship_type_id)),
    });
    let (mail_id, mail_subject) = if victim_char_id > 0 {
        let mid = snap.alloc_entity_id();
        let subject = format!("Insurance Payout for Ship {ship_id}");
        snap.mails.push(eve_db::MailRecord {
            message_id: mid,
            sender_id: 1_000_132, // Secure Commerce Commission
            recipient_id: victim_char_id,
            subject: subject.clone(),
            body: format!(
                "Pend Insurance has credited {:.2} ISK following the destruction of ship {} (type {}).",
                payout_isk, ship_id, ship_type_id
            ),
        });
        snap.mail_timestamps.insert(mid, now_unix);
        snap.mail_assigned_labels.insert((victim_char_id, mid), vec![1]);
        (Some(mid), subject)
    } else {
        (None, String::new())
    };
    ShipInsuranceSettlement {
        payout_isk,
        is_corp,
        credited_owner_id: jid_target,
        balance_after_isk: ba.to_wire_f64(),
        mail_id,
        mail_subject,
    }
}

/// Processes ship insurance payout upon destruction (`feat:combat.insurance-payout`).
/// Pays the insured contract amount (or 40% Basic payout if uninsured), removes the contract,
/// and voids all payout (`0.0`) when destroyed by CONCORD as a criminal.
pub fn process_ship_insurance_payout(
    ctx: &ServerContext,
    victim_char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    concord_killed_criminal: bool,
) -> f64 {
    let now_unix = ctx.now_unix();
    let no_payout = concord_killed_criminal || ship_type_id == 670;
    let settlement = ctx.db.with_mut_snapshot(|snap| {
        settle_ship_insurance_payout_in_snap(
            snap,
            &ctx.sde,
            victim_char_id,
            ship_id,
            ship_type_id,
            no_payout,
            false,
            now_unix,
        )
    });
    if settlement.payout_isk <= 0.0 {
        return 0.0;
    }
    if let Some(mid) = settlement.mail_id {
        let sent = crate::services::mail_notify::SentMail {
            message_id: mid,
            sender_id: 1_000_132,
            to_character_ids: &[victim_char_id],
            to_list_id: None,
            to_corp_or_alliance_id: None,
            title: &settlement.mail_subject,
            status_mask: crate::services::mail_notify::MAIL_STATUS_AUTOMATED,
        };
        crate::services::mail_notify::emit_mail_sent(ctx, &sent, &[victim_char_id]);
    }
    settlement.payout_isk
}

pub fn compute_killmail_esi_hash(
    victim_char_id: i64,
    final_blow_attacker_id: i64,
    victim_ship_type_id: u32,
    kill_time_unix: u64,
) -> String {
    let kill_time_ft = (kill_time_unix as u128) * 10_000_000 + 116_444_736_000_000_000;
    let payload = format!(
        "{}{}{}{}",
        if victim_char_id > 0 {
            victim_char_id.to_string()
        } else {
            "None".to_string()
        },
        if final_blow_attacker_id > 0 {
            final_blow_attacker_id.to_string()
        } else {
            "None".to_string()
        },
        victim_ship_type_id,
        kill_time_ft
    );
    sha1_hex(payload.as_bytes())
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    format!("{h0:08x}{h1:08x}{h2:08x}{h3:08x}{h4:08x}")
}

/// Evaluates the effective signature radius of a ship in space (`feat:dogma.signature-radius`),
/// combining base hull signature radius (`attr 552`), active Microwarpdrive bloom (`attr 554`),

/// Records an offensive weapon or 0-damage EWAR/tackle action against `victim_id` for the 15-minute killmail assist log (`feat:combat.killmail-assist-timeout-ewar`).
pub fn record_killmail_assist_action(
    ctx: &ServerContext,
    victim_id: i64,
    attacker_char_id: i64,
    ship_type_id: u32,
    weapon_type_id: u32,
    damage_done: f64,
    is_ewar: bool,
    action_unix: u64,
) {
    let (corp_id, alliance_id, attacker_ship_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&attacker_char_id)
                .map(|c| (c.corporation_id, c.alliance_id, c.active_ship_id))
                .unwrap_or((1_000_125, None, attacker_char_id))
        });
    let is_drone_assist = ship_type_id == weapon_type_id
        && ctx.sde.types.get(&weapon_type_id).is_some_and(|t| t.category_id == 18);
    if !is_drone_assist {
        crate::services::combat_crimewatch::combat_npc_ai::record_npc_threat_event(
            ctx,
            victim_id,
            attacker_ship_id,
            damage_done,
            is_ewar,
            false,
        );
    }
    with_mut_combat_runtime(ctx, |rt| {
        let entries = rt.aggression_assist_log.entry(victim_id).or_default();
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.attacker_char_id == attacker_char_id)
        {
            existing.damage_done += damage_done.max(0.0);
            existing.weapon_type_id = weapon_type_id;
            existing.ship_type_id = ship_type_id;
            existing.is_ewar = existing.is_ewar || is_ewar;
            existing.last_action_unix = action_unix;
        } else {
            entries.push(AssistLogEntry {
                attacker_char_id,
                corporation_id: corp_id,
                alliance_id,
                ship_type_id,
                weapon_type_id,
                damage_done: damage_done.max(0.0),
                is_ewar,
                last_action_unix: action_unix,
            });
        }
    });
}


/// Registers a spawned wreck (`26557`, 2h lifetime) or jetcan (`23`, 1h lifetime) for decay tracking (`feat:combat.wreck-lifetime`).
pub fn register_container_lifetime(
    ctx: &ServerContext,
    item_id: i64,
    solar_system_id: u32,
    type_id: u32,
    owner_id: i64,
    created_unix: u64,
    is_abandoned: bool,
) {
    let base_lifetime_secs = if type_id == 23 {
        JETCAN_DECAY_LIFETIME_SECS
    } else if ctx.sde.types.get(&type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0) {
        crate::sp6_lifecycle::SECURE_CONTAINER_DECAY_SECS
    } else {
        WRECK_DECAY_LIFETIME_SECS
    };
    let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, solar_system_id)
        .clamp(0.10, 1.0);
    let lifetime_secs = if tidi < 0.995 {
        ((base_lifetime_secs as f64) / tidi).ceil().max(1.0) as u64
    } else {
        base_lifetime_secs
    };
    ctx.db.with_mut_snapshot(|snap| {
        if snap.items.contains_key(&item_id) {
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap,
                item_id,
                created_unix,
                false,
            );
        }
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.insert(
            item_id,
            ContainerLifetimeEntry {
                item_id,
                solar_system_id,
                type_id,
                owner_id,
                created_unix,
                lifetime_secs,
                is_abandoned,
            },
        );
    });
}

pub use crate::services::inventory_stacking::despawn_empty_jetcan_if_needed;

/// Despawns wrecks (> 7200s = 2h), jetcans (> 3600s = 1h), and anchored Mobile Warp Disruptors
/// (> 48h SDE decay lifetime) whose lifetime has expired (`feat:combat.wreck-lifetime`,
/// `feat:lifecycle.mobile_warp_disruptor-lifecycle`).
pub async fn step_wreck_and_jetcan_decay(
    ctx: &ServerContext,
    solar_system_id: u32,
    now_unix: u64,
) -> Vec<i64> {
    let has_expired = with_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.values().any(|entry| {
            entry.solar_system_id == solar_system_id
                && now_unix >= entry.created_unix.saturating_add(entry.lifetime_secs)
        })
    });
    let mut expired: Vec<i64> = if has_expired {
        with_mut_combat_runtime(ctx, |rt| {
            let mut exp = Vec::new();
            for (&iid, entry) in &rt.container_lifetimes {
                if entry.solar_system_id == solar_system_id
                    && now_unix >= entry.created_unix.saturating_add(entry.lifetime_secs)
                {
                    exp.push(iid);
                }
            }
            for iid in &exp {
                rt.container_lifetimes.remove(iid);
            }
            exp
        })
    } else {
        Vec::new()
    };
    let mut expired_bubbles: Vec<i64> = Vec::new();
    ctx.db.with_snapshot(|snap| {
        for item in snap.items.values() {
            if item.location_id != solar_system_id as i64 {
                continue;
            }
            let group_id = ctx
                .sde
                .types
                .get(&item.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            if group_id == 361 {
                if let Some(start_u) = snap
                    .sp6_lifecycle
                    .space_object_meta
                    .get(&item.item_id)
                    .and_then(|m| m.anchor_start_unix.or(m.created_unix))
                {
                    let decay_secs = crate::sp6_lifecycle::sde_mobile_warp_disruptor_decay_secs(
                        &ctx.sde,
                        item.type_id,
                    );
                    if now_unix >= start_u.saturating_add(decay_secs) {
                        expired_bubbles.push(item.item_id);
                    }
                }
            }
        }
    });
    for bid in &expired_bubbles {
        if !expired.contains(bid) {
            expired.push(*bid);
        }
    }
    if !expired.is_empty() {
        if let Some(sys_handle) = ctx.systems.get_handle(solar_system_id) {
            for &bid in &expired_bubbles {
                let _ = sys_handle.remove_warp_bubble(bid).await;
            }
            for &iid in &expired {
                let _ = sys_handle.remove_ball(iid).await;
            }
        }
        ctx.db.with_mut_snapshot(|snap| {
            for &iid in &expired {
                snap.items.remove(&iid);
                snap.item_positions.remove(&iid);
                snap.sp6_lifecycle.space_object_meta.remove(&iid);
                snap.sp6_lifecycle.ship_combat_hp.remove(&iid);
                let children: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|c| c.location_id == iid)
                    .map(|c| c.item_id)
                    .collect();
                for cid in children {
                    snap.items.remove(&cid);
                }
            }
        });
    }
    expired
}

/// Processes a combat ship destruction, computing the hull + fitted item value from SDE and recording the killmail.
pub fn process_ship_destruction_with_aggression(
    ctx: &ServerContext,
    victim_char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    attacker_char_id: i64,
    _weapon_type_id: u32,
    solar_system_id: u32,
    _position: DVec3,
    _is_illegal: bool,
) -> i64 {
    crate::services::inventory_fitting::fitting_cyno_portal::on_cyno_ship_destroyed(ctx, ship_id);
    crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(ctx, ship_id);
    let hull_val = ctx.sde.types.get(&ship_type_id).map(|t| t.base_price).unwrap_or(250_000.0).max(250_000.0);
    let now_unix = ctx.now_unix();
    let kill_id = ctx.db.with_mut_snapshot(|snap| {
        let fitted_val: f64 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id)
            .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.base_price).unwrap_or(1_000.0) * (i.quantity.max(1) as f64))
            .sum();
        snap.next_entity_id += 1;
        let kill_id = snap.next_entity_id;
        snap.record_killmail(
            KillmailRecord {
                kill_id,
                victim_character_id: victim_char_id,
                attacker_character_id: attacker_char_id,
                solar_system_id,
                ship_type_id,
                total_value_isk: hull_val + fitted_val,
                ..Default::default()
            },
            now_unix,
        );
        kill_id
    });
    crate::services::fw_gameplay::on_player_ship_destroyed_in_combat(
        ctx,
        solar_system_id,
        attacker_char_id,
        victim_char_id,
        ship_type_id,
    );
    kill_id
}

const BLUETIME_TICKS_PER_SEC: i64 = 10_000_000;

/// Formats the V24 `what` argument (`(UE_TYPEID, type_id)` or `"ship"` fallback) for self-destruct notices.
fn self_destruct_what_arg(ship_type_id: u32) -> PyRep {
    if ship_type_id > 0 {
        PyRep::Tuple(vec![
            PyRep::Integer(4), // const.UE_TYPEID
            PyRep::Integer(i64::from(ship_type_id)),
        ])
    } else {
        PyRep::String("ship".into())
    }
}

/// Emits a typed `OnRemoteMessage` notification (`msg_id`, `dict`) to `target_char_id`.
fn emit_self_destruct_remote_notice(
    ctx: &ServerContext,
    target_char_id: i64,
    msg_id: &str,
    entries: Vec<(&str, PyRep)>,
) {
    let dict = PyRep::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (PyRep::String(k.to_string()), v))
            .collect(),
    );
    ctx.emit_typed(
        crate::router::cache_notifications::NotificationTarget::Character(target_char_id),
        crate::router::notify_registry::OnRemoteMessage {
            msg_id: msg_id.to_string(),
            dict: Some(dict),
            kw: None,
        },
    );
}

/// Emits V24 `SelfDestructInitiated` (pilot) and `SelfDestructInitiatedOther` (grid observers).
fn emit_self_destruct_initiated_notices(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    remaining_secs: u64,
) {
    let what = self_destruct_what_arg(ship_type_id);
    emit_self_destruct_remote_notice(
        ctx,
        char_id,
        "SelfDestructInitiated",
        vec![
            ("what", what),
            ("when", PyRep::Integer(remaining_secs as i64)),
        ],
    );
    let time_remaining_ticks = (remaining_secs as i64).saturating_mul(BLUETIME_TICKS_PER_SEC);
    for (observer_char_id, _) in ctx.space_observer_chars_for_ship(ship_id, char_id) {
        emit_self_destruct_remote_notice(
            ctx,
            observer_char_id,
            "SelfDestructInitiatedOther",
            vec![
                ("charID", PyRep::Integer(char_id)),
                ("shipTypeID", PyRep::Integer(i64::from(ship_type_id))),
                ("timeRemaining", PyRep::Integer(time_remaining_ticks)),
            ],
        );
    }
}

/// Emits V24 `SelfDestructAborted2` (pilot) and `SelfDestructAbortedOther2` (grid observers).
fn emit_self_destruct_aborted_notices(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    remaining_secs: u64,
) {
    let time_left_ticks = (remaining_secs as i64).saturating_mul(BLUETIME_TICKS_PER_SEC);
    emit_self_destruct_remote_notice(
        ctx,
        char_id,
        "SelfDestructAborted2",
        vec![("timeLeft", PyRep::Integer(time_left_ticks))],
    );
    for (observer_char_id, _) in ctx.space_observer_chars_for_ship(ship_id, char_id) {
        emit_self_destruct_remote_notice(
            ctx,
            observer_char_id,
            "SelfDestructAbortedOther2",
            vec![
                ("charID", PyRep::Integer(char_id)),
                ("timeLeft", PyRep::Integer(time_left_ticks)),
            ],
        );
    }
}

/// Captures `(ship_type_id, online_grid_observers)` before a self-destruct mutation alters or despawns the hull.
pub(crate) fn collect_self_destruct_observer_context(
    ctx: &ServerContext,
    ship_id: i64,
    char_id: i64,
) -> (u32, Vec<(i64, u64)>) {
    let ship_type_id = ctx.db.with_snapshot(|snap| {
        snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(670)
    });
    let pre_detonate_observers = ctx.space_observer_chars_for_ship(ship_id, char_id);
    (ship_type_id, pre_detonate_observers)
}

/// Emits V24 `SelfDestructImmediate` (pilot) and `SelfDestructImmediateOther` (pre-captured grid observers).
pub(crate) fn emit_self_destruct_immediate_notices(
    ctx: &ServerContext,
    char_id: i64,
    ship_type_id: u32,
    pre_detonate_observers: &[(i64, u64)],
) {
    if char_id <= 0 {
        return;
    }
    emit_self_destruct_remote_notice(
        ctx,
        char_id,
        "SelfDestructImmediate",
        vec![("shipTypeID", PyRep::Integer(i64::from(ship_type_id)))],
    );
    for &(observer_char_id, _) in pre_detonate_observers {
        if observer_char_id > 0 && observer_char_id != char_id {
            emit_self_destruct_remote_notice(
                ctx,
                observer_char_id,
                "SelfDestructImmediateOther",
                vec![
                    ("charID", PyRep::Integer(char_id)),
                    ("shipTypeID", PyRep::Integer(i64::from(ship_type_id))),
                ],
            );
        }
    }
}

/// Validates optional `args[0]` / `kwargs["pickid"]` against `expected_ship_id` (`P2-2`).
fn validate_optional_self_destruct_pickid(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    expected_ship_id: i64,
) -> Result<(), BridgeRpcError> {
    if let Some(target_ship_id) = RpcArgs::new(args, kwargs).opt_positive_i64(0, "pickid")? {
        if target_ship_id != expected_ship_id {
            let message = format!(
                "Self-destruct target ship {target_ship_id} does not match active ship {expected_ship_id}"
            );
            return Err(BridgeRpcError::with_client_error(
                "InvalidShip",
                message.clone(),
                eve_proto::ClientError::CustomInfo(format!("InvalidShip: {message}")),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SelfDestructErrorCode {
    CannotSelfDestructWhileDocked,
    NoActiveShip,
    DestinyQueryFailed,
}

impl SelfDestructErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::CannotSelfDestructWhileDocked => "CannotSelfDestructWhileDocked",
            Self::NoActiveShip => "NoActiveShip",
            Self::DestinyQueryFailed => "DestinyQueryFailed",
        }
    }
}

fn self_destruct_rpc_error(
    code: SelfDestructErrorCode,
    message: impl Into<String>,
) -> BridgeRpcError {
    BridgeRpcError {
        code: code.as_str().to_string(),
        message: message.into(),
        client_error: None,
    }
}

fn cannot_self_destruct_docked_err() -> BridgeRpcError {
    self_destruct_rpc_error(
        SelfDestructErrorCode::CannotSelfDestructWhileDocked,
        "Cannot self-destruct a ship while docked in a station or structure",
    )
}

fn no_active_ship_err(message: &str) -> BridgeRpcError {
    self_destruct_rpc_error(SelfDestructErrorCode::NoActiveShip, message)
}

fn validate_self_destruct_snap_preconditions(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
) -> Result<u32, BridgeRpcError> {
    let Some(ch) = snap.characters.get(&char_id) else {
        return Err(no_active_ship_err("Character not found"));
    };
    if ch.station_id.is_some() || ch.structure_id.is_some() {
        return Err(cannot_self_destruct_docked_err());
    }
    if ch.solar_system_id != sys_id || ch.active_ship_id != ship_id {
        return Err(no_active_ship_err(
            "Session ship is no longer active in this solar system",
        ));
    }
    let Some(ship_item) = snap.items.get(&ship_id) else {
        return Err(no_active_ship_err("Active ship item not found"));
    };
    if ship_item.owner_id != char_id || ship_item.location_id != i64::from(sys_id) {
        return Err(no_active_ship_err(
            "Ship is not owned by pilot in this solar system",
        ));
    }
    Ok(ship_item.type_id)
}

/// Advances in-flight missiles and active Breacher Pod DoTs in `solar_system_id` by `dt` seconds


pub async fn handle_wrecks_killmails(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // rpc-lint: allow legacy combat_death_crimewatch self-destruct alias
        ("ship", "SelfDestruct") | ("beyonce", "CmdSelfDestruct") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(cannot_self_destruct_docked_err()));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(no_active_ship_err("Session has no active ship")));
            };
            if let Err(e) = validate_optional_self_destruct_pickid(args, kwargs, ship_id) {
                return Some(Err(e));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no solar system".into(),
                    client_error: None,
                }));
            };
            if let Err(e) = ctx.db.with_snapshot(|snap| {
                validate_self_destruct_snap_preconditions(snap, char_id, sys_id, ship_id)
            }) {
                return Some(Err(e));
            }
            let Some(sys_handle) = ctx.systems.get_handle(sys_id) else {
                return Some(Err(BridgeRpcError::ship_not_in_space(
                    ship_id,
                    Some(char_id),
                    Some(sys_id),
                    "wrecks_killmails",
                    "Active ship is not spawned in space",
                )));
            };
            match crate::services::space_nav_movement_guard::is_ship_module_cloaked_fail_closed(
                ctx,
                &sys_handle,
                char_id,
                sys_id,
                ship_id,
            )
            .await
            {
                Ok(true) => {
                    return Some(Err(BridgeRpcError::from_user_key(
                        eve_proto::UserErrorKey::ShipCloaked,
                    )));
                }
                Ok(false) => {}
                Err(e) => {
                    return Some(Err(self_destruct_rpc_error(
                        SelfDestructErrorCode::DestinyQueryFailed,
                        e,
                    )));
                }
            }
            let now_unix = ctx.now_unix();
            let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, sys_id)
                .clamp(0.10, 1.0);
            enum SelfDestructRpcAction {
                Active {
                    remaining: u64,
                    detonate_at: u64,
                    started_fresh: bool,
                    ship_type_id: u32,
                },
                Detonate {
                    expected_generation: u64,
                },
            }
            let decision: Result<SelfDestructRpcAction, BridgeRpcError> =
                crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
                    &ctx.db,
                    &*ctx.combat_ephemeral,
                    |snap, rt| {
                        let ship_type_id = validate_self_destruct_snap_preconditions(
                            snap, char_id, sys_id, ship_id,
                        )?;

                        let rt_existing = rt.self_destruct_countdowns.get(&ship_id).cloned();
                        let sp6_existing = snap.sp6_lifecycle.self_destructs.get(&ship_id).cloned();
                        let existing = rt_existing.clone().or_else(|| {
                            sp6_existing.as_ref().map(|e| SelfDestructCountdown {
                                ship_id: e.ship_id,
                                char_id: e.char_id,
                                solar_system_id: e.solar_system_id,
                                started_unix: e.started_unix,
                                detonate_at_unix: e.detonate_at_unix,
                            })
                        });

                        if let Some(cd) = existing {
                            if sp6_existing.is_none() {
                                snap.sp6_lifecycle.as_inner_mut().self_destructs.insert(
                                    ship_id,
                                    eve_db::SelfDestructPersistedEntry {
                                        ship_id: cd.ship_id,
                                        char_id: cd.char_id,
                                        solar_system_id: cd.solar_system_id,
                                        started_unix: cd.started_unix,
                                        detonate_at_unix: cd.detonate_at_unix,
                                    },
                                );
                            }
                            if rt_existing.is_none() {
                                rt.self_destruct_countdowns.insert(ship_id, cd.clone());
                            }
                            if now_unix < cd.detonate_at_unix {
                                let remaining = cd.detonate_at_unix.saturating_sub(now_unix);
                                return Ok(SelfDestructRpcAction::Active {
                                    remaining,
                                    detonate_at: cd.detonate_at_unix,
                                    started_fresh: false,
                                    ship_type_id,
                                });
                            }
                            return Ok(SelfDestructRpcAction::Detonate {
                                expected_generation: rt.self_destruct_generation(ship_id),
                            });
                        }

                        let scaled_secs = if tidi < 0.995 {
                            ((SELF_DESTRUCT_COUNTDOWN_SECS as f64) / tidi).ceil().max(1.0) as u64
                        } else {
                            SELF_DESTRUCT_COUNTDOWN_SECS
                        };
                        let detonate_at = now_unix.saturating_add(scaled_secs);
                        rt.bump_self_destruct_generation(ship_id);
                        rt.self_destruct_sim_remaining_secs
                            .insert(ship_id, SELF_DESTRUCT_COUNTDOWN_SECS as f64);
                        rt.self_destruct_countdowns.insert(
                            ship_id,
                            SelfDestructCountdown {
                                ship_id,
                                char_id,
                                solar_system_id: sys_id,
                                started_unix: now_unix,
                                detonate_at_unix: detonate_at,
                            },
                        );
                        snap.sp6_lifecycle.as_inner_mut().self_destructs.insert(
                            ship_id,
                            eve_db::SelfDestructPersistedEntry {
                                ship_id,
                                char_id,
                                solar_system_id: sys_id,
                                started_unix: now_unix,
                                detonate_at_unix: detonate_at,
                            },
                        );
                        Ok(SelfDestructRpcAction::Active {
                            remaining: scaled_secs,
                            detonate_at,
                            started_fresh: true,
                            ship_type_id,
                        })
                    },
                );

            let expected_generation = match decision {
                Ok(SelfDestructRpcAction::Active {
                    remaining,
                    detonate_at,
                    started_fresh,
                    ship_type_id,
                }) => {
                    if started_fresh {
                        emit_self_destruct_initiated_notices(
                            ctx,
                            char_id,
                            ship_id,
                            ship_type_id,
                            remaining,
                        );
                    }
                    return Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(ship_id)),
                        ("countdownStarted", PyRep::Bool(true)),
                        ("detonated", PyRep::Bool(false)),
                        ("remainingSeconds", PyRep::Integer(remaining as i64)),
                        ("detonateAtUnix", PyRep::Integer(detonate_at as i64)),
                    ])));
                }
                Ok(SelfDestructRpcAction::Detonate {
                    expected_generation,
                }) => expected_generation,
                Err(err) => return Some(Err(err)),
            };

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            let (ship_type_id, is_capsule) = ctx.db.with_snapshot(|snap| {
                let stid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(670);
                (stid, stid == 670)
            });

            let res = crate::combat::death_pipeline::execute_self_destruct_guarded(
                ctx,
                sys_id,
                char_id,
                ship_id,
                pos,
                now_unix,
                now_unix,
                Some(expected_generation),
            );

            match res {
                Ok(Some(death)) => {
                    let corpse_item_id: i64 = if is_capsule {
                        death.wreck_item_id
                    } else {
                        register_container_lifetime(
                            ctx,
                            death.wreck_item_id,
                            sys_id,
                            26557,
                            char_id,
                            now_unix,
                            false,
                        );
                        0
                    };

                    // Build DetailedKillmail including any attackers within the 15-minute (900s) assist window
                    let recent_assists: Vec<AssistLogEntry> = with_mut_combat_runtime(ctx, |rt| {
                        let mut valid = Vec::new();
                        for key in [ship_id, char_id] {
                            if let Some(list) = rt.aggression_assist_log.remove(&key) {
                                for e in list {
                                    if now_unix.saturating_sub(e.last_action_unix)
                                        <= KILLMAIL_ASSIST_WINDOW_SECS
                                        && !valid
                                            .iter()
                                            .any(|v: &AssistLogEntry| v.attacker_char_id == e.attacker_char_id)
                                    {
                                        valid.push(e);
                                    }
                                }
                            }
                        }
                        valid
                    });

                    let final_blow_char_id = recent_assists
                        .last()
                        .map(|a| a.attacker_char_id)
                        .unwrap_or(char_id);
                    let km_hash = compute_killmail_esi_hash(
                        char_id,
                        final_blow_char_id,
                        ship_type_id,
                        now_unix,
                    );
                    if !recent_assists.is_empty() {
                        let hull_cs = ctx.build_type_combat_state(ship_type_id);
                        let hull_hp = (hull_cs.max_shield_hp + hull_cs.max_armor_hp + hull_cs.max_structure_hp).max(1.0);
                        let mut km_attackers = Vec::new();
                        for a in &recent_assists {
                            km_attackers.push(KillmailAttackerEntry {
                                character_id: a.attacker_char_id,
                                corporation_id: a.corporation_id,
                                alliance_id: a.alliance_id,
                                ship_type_id: a.ship_type_id,
                                weapon_type_id: a.weapon_type_id,
                                damage_done: a.damage_done,
                                final_blow: a.attacker_char_id == final_blow_char_id,
                                security_status: 0.0,
                            });
                        }
                        let assist_dmg: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
                        ctx.db.with_mut_snapshot(|snap| {
                            if let Some(km) = snap.killmails.get_mut(&death.killmail_id) {
                                km.attacker_character_id = final_blow_char_id;
                            }
                            if let Some(dkm) = snap
                                .combat_crimewatch_state
                                .as_inner_mut()
                                .detailed_killmails
                                .get_mut(&death.killmail_id)
                            {
                                dkm.victim_damage_taken = assist_dmg.max(hull_hp);
                                dkm.attackers = km_attackers;
                                dkm.killmail_hash = km_hash.clone();
                            }
                        });
                    }

                    if let Some(cap_id) = death.ejected_capsule_id {
                        let _ = ctx.execute_session_transition_sync(
                            Some(session),
                            crate::router::SessionTransitionSpec::for_kind(
                                char_id,
                                crate::router::TransitionKind::EjectToPod { capsule_id: cap_id },
                                "self_destruct",
                            )
                            .with_apply_db(false)
                            .with_emit_on_session_changed(false),
                        );
                    } else if let Ok(ch) = ctx.db.get_character(char_id) {
                        crate::router::apply_session_from_character(
                            session,
                            &ch,
                            None,
                            Some(&ctx.sde),
                        );
                        crate::services::map_service::record_solar_system_visit(ctx, char_id, ch.solar_system_id);
                    }
                    Some(Ok(PyRep::keyval([
                        ("wreckID", PyRep::Integer(death.wreck_item_id)),
                        ("corpseID", PyRep::Integer(corpse_item_id)),
                        ("killmailID", PyRep::Integer(death.killmail_id)),
                        ("killmailHash", PyRep::String(km_hash)),
                        ("detonated", PyRep::Bool(true)),
                        ("insurancePayoutISK", PyRep::Real(death.insurance_payout_isk)),
                    ])))
                }
                Ok(None) => {
                    let docked = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(&char_id)
                            .is_some_and(|ch| ch.station_id.is_some() || ch.structure_id.is_some())
                    });
                    Some(Err(if docked {
                        BridgeRpcError {
                            code: "CannotSelfDestructWhileDocked".into(),
                            message: "Cannot self-destruct a ship while docked in a station or structure".into(),
                            client_error: None,
                        }
                    } else {
                        BridgeRpcError {
                            code: "SelfDestructAborted".into(),
                            message: "Self-destruct countdown was aborted or invalidated before detonation".into(),
                            client_error: None,
                        }
                    }))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "SelfDestructFailed".into(),
                    message: e,
                    client_error: None,
                })),
            }
        }

        ("ship", "AbortSelfDestruct") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(cannot_self_destruct_docked_err()));
            }
            if let Err(e) = ctx.db.with_snapshot(|snap| {
                if snap
                    .characters
                    .get(&char_id)
                    .is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
                {
                    return Err(cannot_self_destruct_docked_err());
                }
                let sid = session.ship_id.unwrap_or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.active_ship_id)
                        .unwrap_or(0)
                });
                validate_optional_self_destruct_pickid(args, kwargs, sid)
            }) {
                return Some(Err(e));
            }
            let now_unix = ctx.now_unix();
            let (ship_id, aborted, remaining_secs) =
                match crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
                    &ctx.db,
                    &*ctx.combat_ephemeral,
                    |snap, rt| {
                        if snap
                            .characters
                            .get(&char_id)
                            .is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
                        {
                            return Err(cannot_self_destruct_docked_err());
                        }
                        let sid = session.ship_id.unwrap_or_else(|| {
                            snap.characters
                                .get(&char_id)
                                .map(|c| c.active_ship_id)
                                .unwrap_or(0)
                        });
                        validate_optional_self_destruct_pickid(args, kwargs, sid)?;
                        let sp6_entry = snap
                            .sp6_lifecycle
                            .as_inner_mut()
                            .self_destructs
                            .remove(&sid);
                        let sim_rem = rt.self_destruct_sim_remaining_secs.remove(&sid);
                        let rt_entry = rt.self_destruct_countdowns.remove(&sid);
                        let was_active = sp6_entry.is_some() || rt_entry.is_some();
                        if was_active {
                            rt.bump_self_destruct_generation(sid);
                        }
                        let rem = rt_entry
                            .as_ref()
                            .map(|e| e.detonate_at_unix.saturating_sub(now_unix))
                            .or_else(|| {
                                sp6_entry
                                    .as_ref()
                                    .map(|e| e.detonate_at_unix.saturating_sub(now_unix))
                            })
                            .or_else(|| sim_rem.map(|r| r.ceil().max(0.0) as u64))
                            .unwrap_or(0);
                        Ok((sid, was_active, rem))
                    },
                ) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            if aborted {
                emit_self_destruct_aborted_notices(ctx, char_id, ship_id, remaining_secs);
            }
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("aborted", PyRep::Bool(aborted)),
            ])))
        }

        ("dogmaIM", "Deactivate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = match RpcArgs::new(args, kwargs).req_positive_i64(0, "moduleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or(0);
            if !ctx.db.with_snapshot(|s| s.items.get(&module_id).is_some_and(|i| i.owner_id == char_id && i.location_id == ship_id)) {
                return Some(Err(BridgeRpcError { code: "ModuleNotOwnedByShip".into(), message: format!("Module {module_id} is not fitted to active ship {ship_id}"), client_error: None }));
            }
            crate::services::combat_weapons_spool::clear_entropic_spool(ctx, ship_id, module_id);
            if let Some(sys_id) = session.solar_system_id {
                if let Some(sys) = ctx.systems.get_handle(sys_id) {
                    if !sys.request_cycle_deactivation(ship_id, module_id).await.unwrap_or(false) {
                        let _ = sys.remove_module_cycle(ship_id, module_id).await;
                    }
                }
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
                crate::router::ServerContext::clear_persisted_module_overload(
                    snap,
                    ship_id,
                    module_id,
                );
                if let Some(m_item) = snap.items.get_mut(&module_id) {
                    if let Some(t) = ctx.sde.types.get(&m_item.type_id).filter(|t| t.group_id == 2018) {
                        let base_rep = t.default_attributes.get(&84).copied().unwrap_or(0.0);
                        if base_rep > 0.0 { m_item.dynamic_attributes.insert(84, base_rep); } else { m_item.dynamic_attributes.remove(&84); }
                    }
                }
            });
            ctx.invalidate_and_push_fit(ship_id);
            if let Some(sys_id) = session.solar_system_id {
                crate::services::space_destiny::space_nav_travel::sync_tether(ctx, char_id, sys_id, ship_id).await;
                crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
            }
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("active", PyRep::Bool(false)),
                ("spoolMultiplier", PyRep::Real(1.0)),
            ])))
        }

        ("invbroker", "GetDamageForCrystals") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entries = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .is_some_and(|t| matches!(t.group_id, 86 | 375))
                    })
                    .map(|i| {
                        let hp = ctx
                            .sde
                            .types
                            .get(&i.type_id)
                            .and_then(|t| t.default_attributes.get(&9).copied())
                            .unwrap_or(1.0)
                            .max(0.0001);
                        let dmg = i.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
                        (PyRep::Integer(i.item_id), PyRep::Real((dmg / hp).min(1.0)))
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("invbroker", "LootAll") => {
            Some(combat_wrecks_loot_ops::handle_loot_all(ctx, session, args).await)
        }

        ("warStatisticMgr", "GetKillMail") => {
            Some(combat_wrecks_loot_ops::handle_get_killmail(ctx, session, args))
        }


        _ => None,
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_wrecks_killmails(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("ship", "SelfDestruct", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("beyonce", "CmdSelfDestruct", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow legacy combat_death_crimewatch self-destruct alias
    rpc_entry("ship", "AbortSelfDestruct", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("invbroker", "LootAll", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "combat_wrecks_killmails_tests.rs"] mod combat_wrecks_killmails_tests;

