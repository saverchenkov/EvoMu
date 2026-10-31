#!/usr/bin/env python3
"""Import authoritative NPC corporation LP store offer catalogs from CCP Tranquility ESI.

Reads all NPC corporation IDs with non-empty `lp_offer_tables_json` from `data/evomu_sde.sqlite`,
fetches `https://esi.evetech.net/latest/loyalty/stores/{corporation_id}/offers/?datasource=tranquility`,
filters offers whose item types exist in the V24 `sde_types` table, and writes a compact
deduplicated JSON catalog to `data/esi_lp_store_offers.json`.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import pathlib
import sqlite3
import sys
import time
import urllib.error
import urllib.request

ESI_URL_TEMPLATE = (
    "https://esi.evetech.net/latest/loyalty/stores/{corporation_id}/offers/?datasource=tranquility"
)
USER_AGENT = "EvoMu-ESI-LP-Importer/1.0 (+https://github.com/evomu)"


def load_sde_corps_and_types(sde_path: pathlib.Path) -> tuple[list[int], set[int]]:
    conn = sqlite3.connect(str(sde_path))
    try:
        corps = [
            int(row[0])
            for row in conn.execute(
                "SELECT corporation_id FROM sde_npc_corporations "
                "WHERE lp_offer_tables_json IS NOT NULL "
                "AND lp_offer_tables_json != '' "
                "AND lp_offer_tables_json != '[]' "
                "ORDER BY corporation_id ASC"
            )
        ]
        valid_types = {
            int(row[0]) for row in conn.execute("SELECT type_id FROM sde_types")
        }
        return corps, valid_types
    finally:
        conn.close()


def fetch_corp_offers(corp_id: int) -> tuple[int, list[dict]]:
    url = ESI_URL_TEMPLATE.format(corporation_id=corp_id)
    last_err: Exception | None = None
    for attempt in range(4):
        req = urllib.request.Request(
            url,
            headers={
                "Accept": "application/json",
                "User-Agent": USER_AGENT,
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                raw = resp.read().decode("utf-8")
                data = json.loads(raw)
                if not isinstance(data, list):
                    raise ValueError(f"Expected JSON array for corp {corp_id}")
                return corp_id, data
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return corp_id, []
            last_err = exc
            time.sleep(0.5 * (attempt + 1))
        except Exception as exc:  # noqa: BLE001
            last_err = exc
            time.sleep(0.5 * (attempt + 1))
    raise RuntimeError(f"Failed fetching ESI LP offers for {corp_id}: {last_err}")


def normalize_offer(raw: dict, valid_types: set[int]) -> tuple[int, list] | None:
    offer_id = int(raw["offer_id"])
    type_id = int(raw["type_id"])
    quantity = int(raw["quantity"])
    lp_cost = int(raw["lp_cost"])
    isk_cost = int(raw["isk_cost"])
    ak_cost = int(raw.get("ak_cost") or 0)
    if offer_id <= 0 or quantity <= 0 or lp_cost < 0 or isk_cost < 0 or ak_cost < 0:
        return None
    if valid_types and type_id not in valid_types:
        return None
    req_list: list[list[int]] = []
    for req in raw.get("required_items") or []:
        req_tid = int(req["type_id"])
        req_qty = int(req["quantity"])
        if req_qty <= 0 or (valid_types and req_tid not in valid_types):
            return None
        req_list.append([req_tid, req_qty])
    req_list.sort(key=lambda pair: (pair[0], pair[1]))
    return offer_id, [type_id, quantity, lp_cost, isk_cost, ak_cost, req_list]


def main() -> int:
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--sde",
        type=pathlib.Path,
        default=repo_root / "data" / "evomu_sde.sqlite",
        help="Path to local evomu_sde.sqlite",
    )
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        default=repo_root / "data" / "esi_lp_store_offers.json",
        help="Output JSON catalog path",
    )
    parser.add_argument("--workers", type=int, default=16)
    args = parser.parse_args()

    if not args.sde.exists():
        print(f"error: SDE database not found at {args.sde}", file=sys.stderr)
        return 1

    corps, valid_types = load_sde_corps_and_types(args.sde)
    print(f"Fetching Tranquility ESI LP offers for {len(corps)} NPC corporations...")

    fetched: dict[int, list[dict]] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = [pool.submit(fetch_corp_offers, cid) for cid in corps]
        for fut in concurrent.futures.as_completed(futures):
            cid, data = fut.result()
            fetched[cid] = data

    offers_by_id: dict[int, list] = {}
    corp_offers: dict[str, list[int]] = {}
    skipped_untyped = 0

    for cid in sorted(fetched):
        c_oids: list[int] = []
        for raw_offer in fetched[cid]:
            norm = normalize_offer(raw_offer, valid_types)
            if norm is None:
                skipped_untyped += 1
                continue
            oid, payload = norm
            prev = offers_by_id.get(oid)
            if prev is not None and prev != payload:
                raise RuntimeError(
                    f"Conflicting definition for offer_id {oid} in corp {cid}: {prev} vs {payload}"
                )
            offers_by_id[oid] = payload
            c_oids.append(oid)
        c_oids = sorted(set(c_oids))
        if c_oids:
            corp_offers[str(cid)] = c_oids

    ordered_offers = {str(oid): offers_by_id[oid] for oid in sorted(offers_by_id)}
    catalog = {
        "source": ESI_URL_TEMPLATE,
        "offers": ordered_offers,
        "corp_offers": corp_offers,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    serialized = json.dumps(catalog, separators=(",", ":")) + "\n"
    args.out.write_text(serialized, encoding="utf-8")

    total_links = sum(len(v) for v in corp_offers.values())
    print(
        f"Wrote {args.out}: {len(corp_offers)} corps with offers, "
        f"{len(ordered_offers)} unique offers, {total_links} corp-offer mappings "
        f"({skipped_untyped} post-V24 offers skipped, {len(serialized)} bytes)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
