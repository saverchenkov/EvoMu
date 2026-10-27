//! `combat_wrecks_killmails` — Wreck/Jetcan Lifetimes, LootAll, Detailed Killmails, SHA-1 ESI Hash, Crystal Damage & Self-Destruct (`C-1`, `C-3`).
// parity: feat:combat.insurance-quote
// parity: feat:combat.insurance-buy
// parity: feat:combat.insurance-payout
// parity: feat:combat.insurance-contracts
// parity: feat:ships.ship-insurance-link

#![forbid(unsafe_code)]

use super::*;

pub const INSURANCE_CONTRACT_DURATION_SECS: u64 = 84 * 86_400; // 12 weeks (84 days)

/// Computes the 6-level insurance quote table (`0.5` Basic .. `1.0` Platinum) for a ship type (`feat:combat.insurance-quote`).
pub fn compute_ship_insurance_quote_levels(
    ctx: &ServerContext,
    ship_type_id: u32,
) -> (f64, Vec<(f64, &'static str, f64, f64)>) {
    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, ship_type_id);
    let fracs = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
    let raw = compute_insurance_levels(ref_val);
    let levels = raw
        .into_iter()
        .enumerate()
        .map(|(idx, (name, cost, payout))| (fracs[idx], name, cost, payout))
        .collect();
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
    let valid_contract = contract_opt.filter(|c| c.end_unix > now_unix);
    let contract_owner_id = valid_contract.as_ref().map(|c| c.owner_id);
    let contract_payout = valid_contract
        .as_ref()
        .map(|c| c.payout_isk)
        .unwrap_or(db_payout);
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
                    corp.ensure_wallet_divisions();
                    corp.master_wallet = corp.master_wallet.checked_add(payout_cents).unwrap_or(corp.master_wallet);
                    if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 1) { div.balance = corp.master_wallet; }
                    bal_after = Some(corp.master_wallet);
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
    let (corp_id, alliance_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&attacker_char_id)
                .map(|c| (c.corporation_id, c.alliance_id))
                .unwrap_or((1_000_125, None))
        });
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

/// Despawns wrecks (> 7200s = 2h), jetcans (> 3600s = 1h), and anchored Mobile Warp Disruptors
/// (> 48h SDE decay lifetime) whose lifetime has expired (`feat:combat.wreck-lifetime`,
/// `feat:lifecycle.mobile_warp_disruptor-lifecycle`).
pub async fn step_wreck_and_jetcan_decay(
    ctx: &ServerContext,
    solar_system_id: u32,
    now_unix: u64,
) -> Vec<i64> {
    let mut expired: Vec<i64> = with_mut_combat_runtime(ctx, |rt| {
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
    });
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
    let hull_val = ctx.sde.types.get(&ship_type_id).map(|t| t.base_price).unwrap_or(250_000.0).max(250_000.0);
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
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
    })
}

/// Advances in-flight missiles and active Breacher Pod DoTs in `solar_system_id` by `dt` seconds


pub async fn handle_wrecks_killmails(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // rpc-lint: allow legacy combat_death_crimewatch self-destruct alias
        ("ship", "SelfDestruct") | ("beyonce", "CmdSelfDestruct") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let start_countdown = args.first().and_then(|a| a.as_i64()).is_some();
            if start_countdown {
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
                    // parity: feat:lifecycle.self-destruct-server-timer, plan:SP-7
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

            let (ship_type_id, is_capsule, vic_corp, vic_all) =
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
                    let mut corpse_item_id: i64 = 0;
                    if is_capsule {
                        // Spawn a Frozen Corpse / Corpse Male (type_id 25) in space at the pod's coordinates (`feat:combat.capsule-ejection-items`)
                        corpse_item_id = ctx.db.with_mut_snapshot(|snap| {
                            snap.next_entity_id += 1;
                            let cid = snap.next_entity_id;
                            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                                snap, cid, now_unix, false,
                            );
                            snap.items.insert(
                                cid,
                                InventoryItemRecord {
                                    item_id: cid,
                                    type_id: 25, // Corpse Male
                                    owner_id: char_id,
                                    location_id: sys_id as i64,
                                    flag: InventoryFlag::None,
                                    quantity: 1,
                                    singleton: true,
                                    version: 1,
                                    dynamic_attributes: HashMap::new(),
                                    source_mutaplasmid_id: None,
                                    insured_payout_isk: 0.0,
                                    is_blueprint_copy: false,
                                    blueprint_runs_remaining: None,
                                },
                            );
                            snap.item_positions
                                .insert(cid, [pos.x, pos.y, pos.z]);
                            cid
                        });
                        let _ = sys_handle
                            .spawn_ball_with_combat(
                                eve_destiny::Ball {
                                    id: corpse_item_id,
                                    type_id: 25,
                                    position: pos,
                                    velocity: DVec3::ZERO,
                                    max_velocity: 0.0,
                                    inertia: 1.0,
                                    mass: 80.0,
                                    radius: 5.0,
                                    speed_fraction: 0.0,
                                    mode: eve_destiny::MovementMode::Stop,
                                },
                                ctx.build_type_combat_state(25),
                            )
                            .await;
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
                    }

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
                    let hull_cs = ctx.build_type_combat_state(ship_type_id);
                    let hull_hp = (hull_cs.max_shield_hp + hull_cs.max_armor_hp + hull_cs.max_structure_hp).max(1.0);
                    let mut km_attackers = Vec::new();
                    if recent_assists.is_empty() {
                        km_attackers.push(KillmailAttackerEntry {
                            character_id: char_id,
                            corporation_id: vic_corp,
                            alliance_id: vic_all,
                            ship_type_id,
                            weapon_type_id: ship_type_id,
                            damage_done: hull_hp,
                            final_blow: true,
                            security_status: 0.0,
                        });
                    } else {
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
                    }
                    let assist_dmg: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
                    let km_hash = compute_killmail_esi_hash(
                        char_id,
                        final_blow_char_id,
                        ship_type_id,
                        now_unix,
                    );
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

                    if let Some(cap_id) = death.ejected_capsule_id {
                        session.ship_id = Some(cap_id);
                    } else if let Ok(ch) = ctx.db.get_character(char_id) {
                        session.ship_id = Some(ch.active_ship_id);
                        session.station_id = ch.station_id;
                        session.structure_id = None;
                        session.solar_system_id = Some(ch.solar_system_id);
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
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let ship_id = session.ship_id.unwrap_or(0);
            // Reset Entropic Disintegrator spool on deactivation (`feat:modules.turret-precursor`)
            with_mut_combat_runtime(ctx, |rt| {
                rt.entropic_spools.remove(&(ship_id, module_id));
            });
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
            });
            if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                let _ = fit.deactivate_module(module_id, &ctx.sde);
                ctx.store_ship_fitting(ship_id, fit);
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(wreck_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LootAll requires a container/wreck itemID".into(),
                }));
            };
            let now_unix = ctx.now_unix();
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let _ = step_wreck_and_jetcan_decay(ctx, sys_id, now_unix).await;

            let wreck_opt = ctx
                .db
                .with_snapshot(|snap| snap.items.get(&wreck_id).cloned());
            let Some(wreck_item) = wreck_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ContainerNotFound".into(),
                    message: format!("Wreck or container {wreck_id} no longer exists in space"),
                }));
            };
            if wreck_item.location_id != sys_id as i64 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContainerLocation".into(),
                    message: format!("Container {wreck_id} is not in solar system {sys_id}"),
                }));
            }
            let (is_ship_or_station, is_foreign_depot, is_locked_container) = ctx.db.with_snapshot(|snap| {
                let Some(t) = ctx.sde.types.get(&wreck_item.type_id) else {
                    return (false, false, false);
                };
                let ship_or_st = matches!(t.category_id, 3 | 6 | 65);
                let foreign_depot = t.group_id == 1246 && wreck_item.owner_id != char_id;
                let locked_cont = if matches!(t.group_id, 340 | 448) && wreck_item.owner_id != char_id {
                    let st = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state_snap(snap);
                    st.container_passwords.get(&wreck_id).is_some_and(|pw| !pw.is_empty())
                        && !st.unlocked_containers.contains(&(wreck_id, char_id))
                        && !st.unlocked_containers.contains(&(char_id, wreck_id))
                } else {
                    false
                };
                (ship_or_st, foreign_depot, locked_cont)
            });
            if is_ship_or_station {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContainerType".into(),
                    message: format!("Item {wreck_id} (type {}) is not a lootable wreck or cargo container", wreck_item.type_id),
                }));
            }
            if is_foreign_depot {
                return Some(Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: format!("Cannot loot foreign Mobile Depot {wreck_id}"),
                }));
            }
            if is_locked_container {
                return Some(Err(BridgeRpcError {
                    code: "ContainerLocked".into(),
                    message: format!("Secure container {wreck_id} requires a password before looting"),
                }));
            }
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let active_ball_id = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
                .unwrap_or(ship_id);
            let ship_ball_opt = match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(Some(v)) => Some(v),
                _ => sys_handle.get_ball_snapshot(active_ball_id).await.ok().flatten(),
            };
            if let (Some((_, sb)), Ok(Some((_, wb)))) =
                (ship_ball_opt, sys_handle.get_ball_snapshot(wreck_id).await)
            {
                let dist = sb.position.distance(wb.position);
                if dist > 2_500.0 {
                    return Some(Err(BridgeRpcError {
                        code: "OutOfRange".into(),
                        message: format!(
                            "Wreck or container {wreck_id} is {dist:.1} m away (must be within 2,500 m)"
                        ),
                    }));
                }
            }

            // Check if owner currently holds a criminal timer -> auto-abandon (`feat:crimewatch.criminal-wreck-auto-abandon`)
            let owner_is_criminal = ctx
                .crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_create(wreck_item.owner_id)
                .is_criminal(now_unix);
            let is_abandoned = owner_is_criminal
                || with_combat_runtime(ctx, |rt| {
                    rt.container_lifetimes
                        .get(&wreck_id)
                        .map(|c| c.is_abandoned)
                        .unwrap_or(false)
                });
            let same_corp_or_fleet = ctx.db.with_snapshot(|snap| {
                let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
                let c2 = snap
                    .characters
                    .get(&wreck_item.owner_id)
                    .map(|c| c.corporation_id);
                let same_corp = c1.is_some() && c1 == c2 && c1.unwrap_or(0) > 2_000_000;
                let same_fleet = snap
                    .fleets
                    .values()
                    .any(|f| f.members.contains(&char_id) && f.members.contains(&wreck_item.owner_id));
                same_corp || same_fleet
            });

            let mut suspect_triggered = false;
            if (wreck_item.owner_id != char_id || active_ball_id != ship_id)
                && !is_abandoned
                && !same_corp_or_fleet
            {
                let safety = with_combat_runtime(ctx, |rt| {
                    rt.safety_levels
                        .get(&char_id)
                        .copied()
                        .unwrap_or(SafetyLevel::Red)
                });
                if safety == SafetyLevel::Green {
                    return Some(Err(BridgeRpcError {
                        code: "SafetyLevelRestricted".into(),
                        message: "Safety Green blocks looting a foreign wreck that triggers a Suspect flag".into(),
                    }));
                }
                {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let st = cw.get_or_create(char_id);
                    st.suspect_flag_until_unix = now_unix.saturating_add(900);
                }
                let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
                suspect_triggered = true;
            }

            let ship_cap = ctx
                .db
                .with_snapshot(|snap| {
                    snap.items
                        .get(&ship_id)
                        .and_then(|s| ctx.sde.types.get(&s.type_id))
                        .map(|t| t.capacity)
                })
                .unwrap_or(400.0);

            let (looted_ids, remaining_ids) = ctx.db.with_mut_snapshot(|snap| {
                let used_vol: f64 = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                    .map(|i| {
                        let unit_vol = ctx
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.volume)
                            .unwrap_or(1.0);
                        unit_vol * (i.quantity.max(1) as f64)
                    })
                    .sum();
                let mut avail_vol = (ship_cap - used_vol).max(0.0);

                let mut wreck_children: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| i.location_id == wreck_id)
                    .map(|i| i.item_id)
                    .collect();
                wreck_children.sort_unstable();

                let mut looted = Vec::new();
                let mut remaining = Vec::new();
                for cid in wreck_children {
                    let Some(child) = snap.items.get(&cid).cloned() else {
                        continue;
                    };
                    let unit_vol = ctx
                        .sde
                        .types
                        .get(&child.type_id)
                        .map(|t| t.volume)
                        .unwrap_or(1.0)
                        .max(0.0001);
                    let total_vol = unit_vol * (child.quantity.max(1) as f64);
                    if total_vol <= avail_vol + 1e-6 {
                        avail_vol = (avail_vol - total_vol).max(0.0);
                        if let Some(c_mut) = snap.items.get_mut(&cid) {
                            c_mut.location_id = ship_id;
                            c_mut.owner_id = char_id;
                            c_mut.flag = InventoryFlag::Cargo;
                            c_mut.version += 1;
                        }
                        looted.push(cid);
                    } else {
                        let max_units = (avail_vol / unit_vol).floor() as i32;
                        if max_units > 0 && child.quantity > max_units {
                            avail_vol = (avail_vol - (max_units as f64) * unit_vol).max(0.0);
                            if let Some(c_mut) = snap.items.get_mut(&cid) {
                                c_mut.quantity -= max_units;
                                c_mut.version += 1;
                            }
                            snap.next_entity_id += 1;
                            let split_id = snap.next_entity_id;
                            snap.items.insert(
                                split_id,
                                InventoryItemRecord {
                                    item_id: split_id,
                                    type_id: child.type_id,
                                    owner_id: char_id,
                                    location_id: ship_id,
                                    flag: InventoryFlag::Cargo,
                                    quantity: max_units,
                                    singleton: false,
                                    version: 1,
                                    dynamic_attributes: HashMap::new(),
                                    source_mutaplasmid_id: None,
                                    insured_payout_isk: 0.0,
                                    is_blueprint_copy: false,
                                    blueprint_runs_remaining: None,
                                },
                            );
                            looted.push(split_id);
                        }
                        remaining.push(cid);
                    }
                }
                (looted, remaining)
            });

            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnWreckLootAll",
                "charid",
                vec![
                    PyRep::Integer(wreck_id),
                    PyRep::List(looted_ids.iter().copied().map(PyRep::Integer).collect()),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("wreckID", PyRep::Integer(wreck_id)),
                (
                    "lootedItemIDs",
                    PyRep::List(looted_ids.into_iter().map(PyRep::Integer).collect()),
                ),
                (
                    "remainingItemIDs",
                    PyRep::List(remaining_ids.into_iter().map(PyRep::Integer).collect()),
                ),
                ("suspectFlagTriggered", PyRep::Bool(suspect_triggered)),
                ("abandoned", PyRep::Bool(is_abandoned)),
            ])))
        }

        ("warStatisticMgr", "GetKillMail") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(kill_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetKillMail requires killID".into(),
                }));
            };
            let requested_hash = args.get(1).and_then(|a| a.as_str());

            let detailed_opt = with_combat_runtime(ctx, |rt| {
                rt.detailed_killmails.get(&kill_id).cloned()
            });
            if let Some(dkm) = detailed_opt {
                if let Some(req_h) = requested_hash {
                    if !req_h.is_empty() && req_h != dkm.killmail_hash {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidKillmailHash".into(),
                            message: format!("Killmail hash mismatch for kill {kill_id}"),
                        }));
                    }
                }
                let attackers_list: Vec<PyRep> = dkm
                    .attackers
                    .iter()
                    .map(|a| {
                        PyRep::keyval([
                            ("characterID", PyRep::Integer(a.character_id)),
                            ("corporationID", PyRep::Integer(a.corporation_id)),
                            (
                                "allianceID",
                                a.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            ("shipTypeID", PyRep::Integer(a.ship_type_id as i64)),
                            ("weaponTypeID", PyRep::Integer(a.weapon_type_id as i64)),
                            ("damageDone", PyRep::Real(a.damage_done)),
                            ("finalBlow", PyRep::Bool(a.final_blow)),
                            ("securityStatus", PyRep::Real(a.security_status)),
                        ])
                    })
                    .collect();
                let dropped_list: Vec<PyRep> = dkm
                    .dropped_items
                    .iter()
                    .map(|i| {
                        PyRep::keyval([
                            ("typeID", PyRep::Integer(i.type_id as i64)),
                            ("flag", PyRep::Integer(i.flag as i64)),
                            ("qtyDropped", PyRep::Integer(i.qty_dropped as i64)),
                            ("qtyDestroyed", PyRep::Integer(0)),
                            ("singleton", PyRep::Bool(i.singleton)),
                        ])
                    })
                    .collect();
                let destroyed_list: Vec<PyRep> = dkm
                    .destroyed_items
                    .iter()
                    .map(|i| {
                        PyRep::keyval([
                            ("typeID", PyRep::Integer(i.type_id as i64)),
                            ("flag", PyRep::Integer(i.flag as i64)),
                            ("qtyDropped", PyRep::Integer(0)),
                            ("qtyDestroyed", PyRep::Integer(i.qty_destroyed as i64)),
                            ("singleton", PyRep::Bool(i.singleton)),
                        ])
                    })
                    .collect();

                let final_char_id = dkm
                    .attackers
                    .iter()
                    .find(|a| a.final_blow)
                    .or_else(|| dkm.attackers.first())
                    .map(|a| a.character_id)
                    .unwrap_or(dkm.victim_character_id);
                return Some(Ok(PyRep::keyval([
                    ("killID", PyRep::Integer(dkm.kill_id)),
                    ("killTime", PyRep::Integer(dkm.kill_time_unix as i64)),
                    ("solarSystemID", PyRep::Integer(dkm.solar_system_id as i64)),
                    ("hash", PyRep::String(dkm.killmail_hash)),
                    ("victimCharacterID", PyRep::Integer(dkm.victim_character_id)),
                    ("finalCharacterID", PyRep::Integer(final_char_id)),
                    (
                        "victimCorporationID",
                        PyRep::Integer(dkm.victim_corporation_id),
                    ),
                    (
                        "victimAllianceID",
                        dkm.victim_alliance_id
                            .map(PyRep::Integer)
                            .unwrap_or(PyRep::None),
                    ),
                    (
                        "victimShipTypeID",
                        PyRep::Integer(dkm.victim_ship_type_id as i64),
                    ),
                    ("victimDamageTaken", PyRep::Real(dkm.victim_damage_taken)),
                    (
                        "victimPosition",
                        PyRep::Tuple(vec![
                            PyRep::Real(dkm.victim_position[0]),
                            PyRep::Real(dkm.victim_position[1]),
                            PyRep::Real(dkm.victim_position[2]),
                        ]),
                    ),
                    ("attackers", PyRep::List(attackers_list)),
                    ("droppedItems", PyRep::List(dropped_list)),
                    ("destroyedItems", PyRep::List(destroyed_list)),
                    ("totalValueISK", PyRep::Real(dkm.total_value_isk)),
                    ("iskLost", PyRep::Real(dkm.total_value_isk)),
                ])));
            }

            let basic_opt = ctx
                .db
                .with_snapshot(|snap| snap.killmails.get(&kill_id).cloned());
            let Some(km) = basic_opt else {
                return Some(Err(BridgeRpcError {
                    code: "KillmailNotFound".into(),
                    message: format!("Killmail {kill_id} not found"),
                }));
            };
            let kill_ts = if km.killed_unix > 0 { km.killed_unix } else { ctx.now_unix() };
            let hash = compute_killmail_esi_hash(
                km.victim_character_id,
                km.attacker_character_id,
                km.ship_type_id,
                kill_ts,
            );
            if let Some(req_h) = requested_hash {
                if !req_h.is_empty() && req_h != hash {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidKillmailHash".into(),
                        message: format!("Killmail hash mismatch for kill {kill_id}"),
                    }));
                }
            }
            Some(Ok(PyRep::keyval([
                ("killID", PyRep::Integer(km.kill_id)),
                ("killTime", PyRep::Integer(kill_ts as i64)),
                ("solarSystemID", PyRep::Integer(km.solar_system_id as i64)),
                ("hash", PyRep::String(hash)),
                ("victimCharacterID", PyRep::Integer(km.victim_character_id)),
                ("finalCharacterID", PyRep::Integer(km.attacker_character_id)),
                ("victimShipTypeID", PyRep::Integer(km.ship_type_id as i64)),
                (
                    "attackers",
                    PyRep::List(vec![PyRep::keyval([
                        ("characterID", PyRep::Integer(km.attacker_character_id)),
                        ("finalBlow", PyRep::Bool(true)),
                    ])]),
                ),
                ("droppedItems", PyRep::List(Vec::new())),
                ("destroyedItems", PyRep::List(Vec::new())),
                ("totalValueISK", PyRep::Real(km.total_value_isk)),
                ("iskLost", PyRep::Real(km.total_value_isk)),
            ])))
        }


        _ => None,
    }
}
