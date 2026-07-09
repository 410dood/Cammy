#!/usr/bin/env python3
"""Cammy ↔ Lemon Squeezy setup helper.

Automates the store-side wiring that the Lemon Squeezy API actually allows, so
going live is a command instead of a dashboard scavenger hunt:

  * `--check`   verify the API key, show the store, products, variants, webhooks.
  * `--webhook` create (or update) the `order_created` webhook that drives
                `fulfilment_server.py`, generating a signing secret and printing
                the exact env vars to set on the app and the fulfilment server.

What it deliberately does NOT do: create the product. The LS API is read-only
for products/variants (`POST /v1/products` → 405) — you build the $79 product
in the dashboard once, then everything below is automatable.

The API key is read from `$LEMON_SQUEEZY_API_KEY` or `~/.config/cammy/ls_api_key`
(chmod 600). It is never printed.

Examples:
    python3 scripts/ls_setup.py --check
    python3 scripts/ls_setup.py --webhook --url https://buy.cammy.app/ls-webhook
"""
import argparse
import json
import os
import secrets
import sys
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.lemonsqueezy.com/v1"
KEY_FILE = Path.home() / ".config" / "cammy" / "ls_api_key"


def load_key() -> str:
    key = os.environ.get("LEMON_SQUEEZY_API_KEY")
    if not key and KEY_FILE.exists():
        key = KEY_FILE.read_text().strip()
    if not key:
        sys.exit(f"no API key: set $LEMON_SQUEEZY_API_KEY or write it to {KEY_FILE}")
    return key


def api(key: str, method: str, path: str, body: dict | None = None) -> dict:
    url = path if path.startswith("http") else f"{API}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {key}")
    req.add_header("Accept", "application/vnd.api+json")
    if data is not None:
        req.add_header("Content-Type", "application/vnd.api+json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        try:
            detail = json.dumps(json.loads(detail).get("errors", detail), indent=2)
        except Exception:
            pass
        sys.exit(f"LS API {method} {url} -> HTTP {e.code}\n{detail}")


def first_store(key: str) -> dict:
    stores = api(key, "GET", "/stores").get("data", [])
    if not stores:
        sys.exit("no stores on this account")
    return stores[0]


def cmd_check(key: str) -> None:
    store = first_store(key)
    sid = store["id"]
    sa = store["attributes"]
    print(f"store:    {sid}  {sa['name']}  ({sa.get('domain')})")

    prods = api(key, "GET", f"/products?filter[store_id]={sid}").get("data", [])
    print(f"products: {len(prods)}")
    for p in prods:
        pa = p["attributes"]
        print(f"  - id={p['id']}  {pa['name']}  {pa.get('price_formatted','?')}  "
              f"status={pa.get('status')}  buy_now_url={pa.get('buy_now_url')}")
        vs = api(key, "GET", f"/variants?filter[product_id]={p['id']}").get("data", [])
        for v in vs:
            va = v["attributes"]
            print(f"      variant id={v['id']}  {va['name']}  price={va.get('price')}  "
                  f"status={va.get('status')}")

    hooks = api(key, "GET", f"/webhooks?filter[store_id]={sid}").get("data", [])
    print(f"webhooks: {len(hooks)}")
    for w in hooks:
        wa = w["attributes"]
        url = wa.get("url") or "(empty)"
        print(f"  - id={w['id']}  {url}  events={wa.get('events')}  test_mode={wa.get('test_mode')}")

    if not prods:
        print("\nNEXT: create the product in the dashboard (API can't):")
        print("  Store -> Products -> New Product → 'Cammy', single payment $79,")
        print("  then Publish. Re-run --check to see its variant id + buy URL.")
    else:
        print("\nProduct exists. Wire fulfilment with:")
        print("  python3 scripts/ls_setup.py --webhook --url https://YOUR-PUBLIC-HOST/ls-webhook")


def find_order_webhook(key: str, sid: str, url: str) -> dict | None:
    for w in api(key, "GET", f"/webhooks?filter[store_id]={sid}").get("data", []):
        if w["attributes"].get("url") == url:
            return w
    return None


def cmd_webhook(key: str, url: str, secret: str | None) -> None:
    store = first_store(key)
    sid = store["id"]
    secret = secret or secrets.token_hex(20)  # LS signing secret (<=40 chars)
    # order_updated matters too: a delayed-payment order arrives as
    # order_created status=pending and only flips to paid in order_updated —
    # without it that buyer would never be fulfilled.
    events = ["order_created", "order_updated"]

    existing = find_order_webhook(key, sid, url)
    payload = {
        "data": {
            "type": "webhooks",
            "attributes": {"url": url, "events": events, "secret": secret},
            "relationships": {
                "store": {"data": {"type": "stores", "id": str(sid)}}
            },
        }
    }
    if existing:
        payload["data"]["id"] = existing["id"]
        api(key, "PATCH", f"/webhooks/{existing['id']}", payload)
        print(f"updated webhook {existing['id']} -> {url}")
    else:
        created = api(key, "POST", "/webhooks", payload)
        print(f"created webhook {created['data']['id']} -> {url}")

    print("\nSet these — the SAME secret must be on both sides:")
    print(f"  # on the fulfilment server:")
    print(f"  export LEMON_SQUEEZY_WEBHOOK_SECRET={secret}")
    print(f"  # and (optional) point the in-app Buy button at your product page:")
    print(f"  export CAMMY_BUY_URL=<product buy_now_url from --check>")
    print("\nStore the secret safely; it is shown only once here.")


def main() -> None:
    p = argparse.ArgumentParser(description="Cammy ↔ Lemon Squeezy setup helper.")
    p.add_argument("--check", action="store_true", help="show store/products/webhooks (default)")
    p.add_argument("--webhook", action="store_true", help="create/update the order_created webhook")
    p.add_argument("--url", help="public webhook URL (required with --webhook)")
    p.add_argument("--secret", help="use this signing secret instead of generating one")
    args = p.parse_args()

    key = load_key()
    if args.webhook:
        if not args.url:
            p.error("--webhook requires --url")
        cmd_webhook(key, args.url, args.secret)
    else:
        cmd_check(key)


if __name__ == "__main__":
    main()
