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

/// Synchronizes `rt.insurance_contracts` with `snap.items`, creating 12-week contract records for newly insured ships
/// and voiding contracts whose ship was repackaged (`!singleton`), destroyed, or transferred/sold (`feat:ships.ship-insurance-link`, `feat:combat.insurance-contracts`).
pub fn sync_ship_insurance_contracts(ctx: &ServerContext, owner_id: i64) {
    let now_unix = ctx.now_unix();
    let ship_items: Vec<(i64, u32, i64, bool, f64)> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                ctx.sde
                    .types
                    .get(&i.type_id)
                    .map(|t| t.category_id == 6)
                    .unwrap_or(false)
            })
            .map(|i| (i.item_id, i.type_id, i.owner_id, i.singleton, i.insured_payout_isk))
            .collect()
    });
    let container_st = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(ctx);
    let mut voided_ship_ids = Vec::new();
    let new_contracts = with_mut_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.clear();
        for (sid, c) in &container_st.insurance_contracts {
            if c.end_unix <= now_unix {
                voided_ship_ids.push(*sid);
                continue;
            }
            if let Some((_, _, cur_owner, singleton, payout)) =
                ship_items.iter().find(|(id, _, _, _, _)| *id == *sid)
            {
                if !*singleton || *cur_owner != c.owner_id || *payout <= 0.0 {
                    voided_ship_ids.push(*sid);
                    continue;
                }
                rt.insurance_contracts.insert(
                    *sid,
                    InsuranceContractState {
                        ship_id: c.ship_id,
                        ship_type_id: c.ship_type_id,
                        owner_id: c.owner_id,
                        level_name: c.level_name.clone(),
                        level_fraction: c.level_fraction,
                        cost_isk: c.cost_isk,
                        payout_isk: c.payout_isk,
                        start_unix: c.start_unix,
                        end_unix: c.end_unix,
                    },
                );
            } else {
                voided_ship_ids.push(*sid);
            }
        }
        let mut new_contracts: Vec<(i64, crate::services::inventory_fitting::fitting_containers_ships::InsuranceContractRecord)> = Vec::new();
        for &(sid, type_id, item_owner, singleton, payout_isk) in &ship_items {
            if singleton
                && payout_isk > 0.0
                && (item_owner == owner_id || owner_id == 0)
                && !rt.insurance_contracts.contains_key(&sid)
                && !voided_ship_ids.contains(&sid)
            {
                let ref_val = compute_ship_insurance_reference_value(&ctx.sde, type_id);
                let frac = (payout_isk / ref_val.max(1.0)).clamp(0.4, 1.0);
                let (cost_isk, _) = compute_insurance_quote(ref_val, frac);
                let rec = crate::services::inventory_fitting::fitting_containers_ships::InsuranceContractRecord {
                    ship_id: sid,
                    ship_type_id: type_id,
                    owner_id: item_owner,
                    level_name: if frac >= 0.99 { "Platinum".into() } else { "Standard".into() },
                    level_fraction: frac,
                    cost_isk,
                    payout_isk,
                    start_unix: now_unix,
                    end_unix: now_unix.saturating_add(INSURANCE_CONTRACT_DURATION_SECS),
                };
                new_contracts.push((sid, rec));
                rt.insurance_contracts.insert(
                    sid,
                    InsuranceContractState {
                        ship_id: sid,
                        ship_type_id: type_id,
                        owner_id: item_owner,
                        level_name: if frac >= 0.99 { "Platinum".into() } else { "Standard".into() },
                        level_fraction: frac,
                        cost_isk,
                        payout_isk,
                        start_unix: now_unix,
                        end_unix: now_unix.saturating_add(INSURANCE_CONTRACT_DURATION_SECS),
                    },
                );
            }
        }
        new_contracts
    });
    if !new_contracts.is_empty() {
        crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
            for (ship_id, rec) in new_contracts {
                st.insurance_contracts.insert(ship_id, rec);
            }
        });
    }
    if !voided_ship_ids.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for sid in voided_ship_ids.clone() {
                if let Some(it) = snap.items.get_mut(&sid) {
                    it.insured_payout_isk = 0.0;
                }
            }
        });
        crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
            for sid in voided_ship_ids {
                st.insurance_contracts.remove(&sid);
            }
        });
    }
}

/// Voids an active ship insurance contract when the ship is repackaged, traded, or un-insured (`feat:ships.ship-insurance-link`, `feat:combat.insurance-contracts`).
pub fn void_ship_insurance_contract(ctx: &ServerContext, ship_id: i64) -> bool {
    let removed_rt = with_mut_combat_runtime(ctx, |rt| rt.insurance_contracts.remove(&ship_id).is_some());
    let removed_db = ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            let had = it.insured_payout_isk > 0.0;
            it.insured_payout_isk = 0.0;
            had
        } else {
            false
        }
    });
    let removed_container = crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
        st.insurance_contracts.remove(&ship_id).is_some()
    });
    removed_rt || removed_db || removed_container
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
    sync_ship_insurance_contracts(ctx, victim_char_id);
    let now_unix = ctx.now_unix();
    let contract_opt = with_mut_combat_runtime(ctx, |rt| rt.insurance_contracts.remove(&ship_id));
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
        st.insurance_contracts.remove(&ship_id);
    });
    let db_payout = ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            let p = it.insured_payout_isk;
            it.insured_payout_isk = 0.0;
            p
        } else {
            0.0
        }
    });
    if concord_killed_criminal || ship_type_id == 670 {
        return 0.0;
    }
    let valid_contract = contract_opt.as_ref().filter(|c| c.end_unix > now_unix);
    let contract_owner_id = valid_contract.as_ref().map(|c| c.owner_id);
    let contract_payout = match contract_opt {
        Some(ref c) if c.end_unix > now_unix => c.payout_isk,
        Some(_) => 0.0,
        None => db_payout,
    };
    let payout_isk = if contract_payout > 0.0 {
        contract_payout
    } else {
        let ref_val = compute_ship_insurance_reference_value(&ctx.sde, ship_type_id);
        compute_insurance_quote(ref_val, 0.40).1
    };
    if payout_isk > 0.0 {
        let (credited_char, mail_id, credited_any) = ctx.db.with_mut_snapshot(|snap| {
            let payout_cents = IskCents::from_f64_isk(payout_isk);
            let mut credited_char = None;
            let mut bal_after = None;
            let mut journal_target = None;
            let is_victim = contract_owner_id.is_none() || contract_owner_id == Some(victim_char_id);
            if let Some(owner_id) = contract_owner_id {
                if let Some(corp) = snap.corporations.get_mut(&owner_id) {
                    bal_after = Some(corp.credit_division(1, payout_cents));
                    journal_target = Some(owner_id);
                }
            }
            if journal_target.is_none() && is_victim {
                if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
                    ch.balance = ch.balance.checked_add(payout_cents).unwrap_or(ch.balance);
                    bal_after = Some(ch.balance);
                    credited_char = Some((victim_char_id, ch.balance.to_wire_f64()));
                    journal_target = Some(victim_char_id);
                }
            }
            let mut mail_id = None;
            if let (Some(ba), Some(jid_target)) = (bal_after, journal_target) {
                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: jid,
                    character_id: jid_target,
                    entry_type: "insurance_payout".to_string(),
                    amount: payout_cents,
                    balance_after: ba,
                    reason: format!("Insurance payout for loss of ship {ship_id}"),
                });
                snap.next_entity_id += 1;
                let mid = snap.next_entity_id;
                snap.mails.push(eve_db::MailRecord {
                    message_id: mid,
                    sender_id: 1_000_132, // Secure Commerce Commission
                    recipient_id: victim_char_id,
                    subject: format!("Insurance Payout for Ship {ship_id}"),
                    body: format!(
                        "Pend Insurance has credited {:.2} ISK following the destruction of ship {} (type {}).",
                        payout_isk, ship_id, ship_type_id
                    ),
                });
                snap.mail_timestamps.insert(mid, now_unix);
                snap.mail_assigned_labels.insert((victim_char_id, mid), vec![1]);
                mail_id = Some(mid);
            }
            let credited_any = journal_target.is_some();
            (credited_char, mail_id, credited_any)
        });
        if !credited_any { return 0.0; }
        if let Some((cid, bal)) = credited_char {
            let _ = ctx.notify_account_change(cid, "cash", cid, bal);
        }
        if let Some(mid) = mail_id {
            let title = format!("Insurance Payout for Ship {ship_id}");
            let sent = crate::services::mail_notify::SentMail {
                message_id: mid, sender_id: 1_000_132, to_character_ids: &[victim_char_id],
                to_list_id: None, to_corp_or_alliance_id: None, title: &title,
                status_mask: crate::services::mail_notify::MAIL_STATUS_AUTOMATED,
            };
            crate::services::mail_notify::emit_mail_sent(ctx, &sent, &[victim_char_id]);
        }
        return payout_isk;
    }
    0.0
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
    let lifetime_secs = if type_id == 23 {
        JETCAN_DECAY_LIFETIME_SECS
    } else if ctx.sde.types.get(&type_id).is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0) {
        crate::sp6_lifecycle::SECURE_CONTAINER_DECAY_SECS
    } else {
        WRECK_DECAY_LIFETIME_SECS
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
        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        for &bid in &expired_bubbles {
            let _ = sys_handle.remove_warp_bubble(bid).await;
        }
        for &iid in &expired {
            let _ = sys_handle.remove_ball(iid).await;
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
                return Some(Err(BridgeRpcError {
                    code: "CannotSelfDestructWhileDocked".into(),
                    message: "Cannot self-destruct a ship while docked in a station or structure".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no solar system".into(),
                }));
            };
            let now_unix = ctx.now_unix();
            let existing = with_combat_runtime(ctx, |rt| {
                rt.self_destruct_countdowns.get(&ship_id).cloned()
            });
            if let Some(cd) = existing {
                if now_unix < cd.detonate_at_unix {
                    let remaining = cd.detonate_at_unix.saturating_sub(now_unix);
                    return Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(ship_id)),
                        ("countdownStarted", PyRep::Bool(true)),
                        ("detonated", PyRep::Bool(false)),
                        ("remainingSeconds", PyRep::Integer(remaining as i64)),
                        ("detonateAtUnix", PyRep::Integer(cd.detonate_at_unix as i64)),
                    ])));
                }
            } else {
                let detonate_at = now_unix.saturating_add(SELF_DESTRUCT_COUNTDOWN_SECS);
                with_mut_combat_runtime(ctx, |rt| {
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
                });
                crate::sp6_lifecycle::register_self_destruct_countdown(
                    ctx,
                    ship_id,
                    char_id,
                    sys_id,
                    now_unix,
                    detonate_at,
                );
                return Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("countdownStarted", PyRep::Bool(true)),
                    ("detonated", PyRep::Bool(false)),
                    (
                        "remainingSeconds",
                        PyRep::Integer(SELF_DESTRUCT_COUNTDOWN_SECS as i64),
                    ),
                    ("detonateAtUnix", PyRep::Integer(detonate_at as i64)),
                ])));
            }

            with_mut_combat_runtime(ctx, |rt| {
                rt.self_destruct_countdowns.remove(&ship_id);
            });
            let _ = crate::sp6_lifecycle::abort_self_destruct_countdown(ctx, ship_id);

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            let (ship_type_id, is_capsule, _vic_corp, _vic_all) =
                ctx.db.with_snapshot(|snap| {
                    let stid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(670);
                    let (corp, all) = snap
                        .characters
                        .get(&char_id)
                        .map(|c| (c.corporation_id, c.alliance_id))
                        .unwrap_or((1_000_009, None));
                    (stid, stid == 670, corp, all)
                });

            let res = crate::combat::death_pipeline::execute_self_destruct(
                ctx,
                sys_id,
                char_id,
                ship_id,
                pos,
                now_unix,
            )
            .await;

            match res {
                Ok(death) => {
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
                        });
                        with_mut_combat_runtime(ctx, |rt| {
                            if let Some(dkm) = rt.detailed_killmails.get_mut(&death.killmail_id) {
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
                Err(e) => Some(Err(BridgeRpcError {
                    code: "SelfDestructFailed".into(),
                    message: e,
                })),
            }
        }

        ("ship", "AbortSelfDestruct") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
                    .unwrap_or(0)
            });
            let aborted_rt = with_mut_combat_runtime(ctx, |rt| {
                rt.self_destruct_countdowns.remove(&ship_id).is_some()
            });
            let aborted_sp6 = crate::sp6_lifecycle::abort_self_destruct_countdown(ctx, ship_id);
            let aborted = aborted_rt || aborted_sp6;
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
                return Some(Err(BridgeRpcError { code: "ModuleNotOwnedByShip".into(), message: format!("Module {module_id} is not fitted to active ship {ship_id}") }));
            }
            with_mut_combat_runtime(ctx, |rt| { rt.entropic_spools.remove(&(ship_id, module_id)); });
            if let Some(sys_id) = session.solar_system_id {
                let _ = ctx.systems.get_or_spawn(sys_id).remove_module_cycle(ship_id, module_id).await;
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
                if let Some(m_item) = snap.items.get_mut(&module_id) {
                    if let Some(t) = ctx.sde.types.get(&m_item.type_id).filter(|t| t.group_id == 2018) {
                        let base_rep = t.default_attributes.get(&84).copied().unwrap_or(0.0);
                        if base_rep > 0.0 { m_item.dynamic_attributes.insert(84, base_rep); } else { m_item.dynamic_attributes.remove(&84); }
                    }
                }
            });
            if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                let _ = fit.deactivate_module(module_id, &ctx.sde);
                ctx.store_ship_fitting(ship_id, fit);
            }
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

#[cfg(test)]
#[path = "combat_wrecks_killmails_tests.rs"] mod combat_wrecks_killmails_tests;

