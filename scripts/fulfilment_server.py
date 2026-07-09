#!/usr/bin/env python3
"""Cammy license fulfilment server — the automated issuance side of the store.

A buyer checks out on Lemon Squeezy; Lemon Squeezy POSTs an `order_created`
webhook here; this server verifies the signature, signs a license key with the
same logic as `license_sign.py`, and emails it to the buyer. Hands-off selling.

Design goals, in priority order:
  1. Never lose a paid order. If email fails or SMTP is unconfigured, the key is
     spooled to disk and the order is logged LOUDLY — you can always resend.
  2. Never double-issue. A JSON ledger keyed by order id makes delivery
     idempotent; Lemon Squeezy retries webhooks, so this matters.
  3. Never trust an unsigned request. The webhook HMAC is checked in constant
     time before anything else happens.
  4. One source of truth for the key format — imports `build_key` from
     `license_sign.py`, so issuance can't drift from what the app verifies.

Depends only on the stdlib + `cryptography` (already required to sign). No Flask.

── Run ──────────────────────────────────────────────────────────────────────
    export CAMMY_LICENSE_SEED=<hex>              # the offline signing seed
    export LEMON_SQUEEZY_WEBHOOK_SECRET=<secret> # from the LS webhook settings
    # optional SMTP (else keys spool to ./fulfilment-spool/):
    export SMTP_HOST=smtp.example.com SMTP_PORT=587 \
           SMTP_USER=... SMTP_PASS=... SMTP_FROM='Cammy <licenses@cammy.app>'
    python3 scripts/fulfilment_server.py --port 8787

Point a Lemon Squeezy webhook at  https://your-host/…  for the `order_created`
AND `order_updated` events (a delayed-payment order only flips to paid in the
update), using the same signing secret. Put this behind TLS (a reverse proxy).

── Variant mapping ──────────────────────────────────────────────────────────
By default every order issues a `lifetime`, 2-seat key. To sell an update plan
(subscription) as a separate variant, drop a config file and point
CAMMY_FULFILMENT_CONFIG at it:

    {
      "seats": 2,
      "variants": {
        "123456": {"plan": "lifetime"},
        "123457": {"plan": "subscription", "expires_in_days": 365}
      }
    }

Keys are Lemon Squeezy numeric variant ids (strings). Unmapped variants fall
back to the top-level defaults (lifetime).

── Self-test ────────────────────────────────────────────────────────────────
    python3 scripts/fulfilment_server.py --selftest
Runs the whole pipeline in-process against an ephemeral keypair: signs a fake
webhook, feeds it through signature-check → issuance → idempotency, and asserts
the emitted key verifies. No network, no real seed.
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
import smtplib
import sys
import threading
import time
from email.message import EmailMessage
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from license_sign import build_key, load_seed  # noqa: E402


# Default state paths are anchored to this script's directory, NOT the cwd —
# restarting the server from a different directory must not silently start a
# blank ledger (which would let Lemon Squeezy retries double-issue keys).
_HERE = Path(__file__).resolve().parent
SPOOL_DIR = Path(os.environ.get("CAMMY_FULFILMENT_SPOOL", _HERE / "fulfilment-spool"))
LEDGER = Path(os.environ.get("CAMMY_FULFILMENT_LEDGER", _HERE / "fulfilment-ledger.json"))

# ThreadingHTTPServer handles requests concurrently; the ledger read-modify-write
# in fulfil() must be atomic or a concurrent order (or an LS retry racing the
# original) can clobber another order's entry and later double-issue.
_LEDGER_LOCK = threading.Lock()

# Cap the pre-auth body read: the webhook endpoint is internet-facing and reads
# the body before signature verification, so an unauthenticated request must not
# be able to allocate unbounded memory. Real LS payloads are a few KB.
MAX_BODY_BYTES = 1_000_000


# ── issuance ────────────────────────────────────────────────────────────────

def load_config() -> dict:
    path = os.environ.get("CAMMY_FULFILMENT_CONFIG")
    if not path:
        return {}
    try:
        return json.loads(Path(path).read_text())
    except (OSError, json.JSONDecodeError) as e:
        sys.exit(f"bad CAMMY_FULFILMENT_CONFIG ({path}): {e}")


def plan_for_order(cfg: dict, variant_id: str, variant_name: str) -> dict:
    """Resolve (plan, seats, expires) for an order line. Explicit variant-id
    mapping wins; otherwise fall back to name heuristics then to lifetime."""
    default_seats = int(cfg.get("seats", 2))
    variants = cfg.get("variants", {})
    spec = variants.get(str(variant_id))
    if spec is None:
        # No explicit mapping: infer a subscription from the variant name so an
        # unconfigured store still does the sane thing for an update plan.
        name = (variant_name or "").lower()
        if any(w in name for w in ("subscription", "update plan", "annual", "yearly")):
            spec = {"plan": "subscription", "expires_in_days": 365}
        else:
            spec = {"plan": "lifetime"}
    plan = spec.get("plan", "lifetime")
    seats = int(spec.get("seats", default_seats))
    expires = None
    if plan == "subscription":
        days = int(spec.get("expires_in_days", 365))
        expires = int(time.time()) + days * 86400
    return {"plan": plan, "seats": seats, "expires": expires}


def parse_order(body: dict) -> dict | None:
    """Pull the fields we need out of a Lemon Squeezy order webhook payload.
    Returns None if this isn't an order we should fulfil.

    Both `order_created` and `order_updated` are accepted: an order paid with a
    delayed payment method arrives as `order_created` with status `pending` and
    only flips to `paid` in a later `order_updated` — fulfilling on the update
    (idempotent via the ledger) is what keeps that buyer from being dropped."""
    meta = body.get("meta") or {}
    if meta.get("event_name") not in ("order_created", "order_updated"):
        return None
    data = body.get("data") or {}
    attrs = data.get("attributes") or {}
    email = attrs.get("user_email")
    if not email:
        return None
    # A refunded/failed order should not mint a key; a pending one is fulfilled
    # by the order_updated that marks it paid — log it loudly so a stuck pending
    # order is visible even if that update never comes.
    status = (attrs.get("status") or "paid").lower()
    if status == "pending":
        oid = data.get("id") or attrs.get("order_number")
        print(f"[PENDING] order {oid} ({email}) not yet paid — awaiting order_updated", flush=True)
        return None
    if status not in ("paid", "active"):
        return None
    item = attrs.get("first_order_item") or {}
    order_id = str(data.get("id") or attrs.get("identifier") or attrs.get("order_number") or "")
    return {
        "order_id": order_id,
        "order_number": attrs.get("order_number"),
        "email": email,
        "variant_id": item.get("variant_id"),
        "variant_name": item.get("variant_name") or item.get("product_name") or "",
    }


# ── delivery ────────────────────────────────────────────────────────────────

EMAIL_TEMPLATE = """Hi,

Thanks for buying Cammy! Here's your license key:

    {key}

To activate: open Cammy, go to Settings → License, paste the key, and click
Activate. Activation is fully offline — it works even with no internet, and
Cammy never phones home.

Your key is good for {seats} machines{plan_note}. Keep this email; you can
re-activate any time (for example after moving to a new PC — just remove the
license on the old machine first).

Questions? Just reply to this email.

— The Cammy team
"""


def render_email(key: str, seats: int, plan: str, expires: int | None) -> str:
    note = ""
    if plan == "subscription" and expires:
        when = time.strftime("%Y-%m-%d", time.gmtime(expires))
        note = f" and includes the update plan through {when}"
    return EMAIL_TEMPLATE.format(key=key, seats=seats, plan_note=note)


def spool(order_id: str, email: str, key: str, reason: str) -> None:
    SPOOL_DIR.mkdir(parents=True, exist_ok=True)
    safe = "".join(c if c.isalnum() else "_" for c in order_id) or "unknown"
    path = SPOOL_DIR / f"order-{safe}.txt"
    path.write_text(
        f"order: {order_id}\nemail: {email}\nreason: {reason}\nkey:\n{key}\n",
        encoding="utf-8",
    )
    print(f"[SPOOL] order {order_id}: key written to {path} ({reason}) — deliver manually", flush=True)


def deliver_email(to_email: str, body: str) -> bool:
    host = os.environ.get("SMTP_HOST")
    if not host:
        return False
    msg = EmailMessage()
    msg["Subject"] = "Your Cammy license key"
    msg["From"] = os.environ.get("SMTP_FROM", "Cammy <licenses@cammy.app>")
    msg["To"] = to_email
    msg.set_content(body)
    port = int(os.environ.get("SMTP_PORT", "587"))
    user = os.environ.get("SMTP_USER")
    pw = os.environ.get("SMTP_PASS")
    try:
        if port == 465:
            server = smtplib.SMTP_SSL(host, port, timeout=30)
        else:
            server = smtplib.SMTP(host, port, timeout=30)
            server.starttls()
        with server:
            if user:
                server.login(user, pw or "")
            server.send_message(msg)
        return True
    except Exception as e:  # network/auth/etc — caller spools instead
        print(f"[SMTP] send to {to_email} failed: {e}", flush=True)
        return False


# ── ledger (idempotency) ────────────────────────────────────────────────────

def ledger_read() -> dict:
    if not LEDGER.exists():
        return {}
    # An EXISTING ledger that cannot be read or parsed must be a hard error, not
    # an empty dict — returning {} here would let the next write atomically
    # replace the whole issuance history with a single order. The caller's
    # crash-spool path preserves the in-flight order; a human fixes the file.
    try:
        return json.loads(LEDGER.read_text())
    except (OSError, json.JSONDecodeError) as e:
        raise RuntimeError(
            f"ledger {LEDGER} exists but is unreadable/corrupt ({e}) — "
            "refusing to continue, fix or move it"
        ) from e


def ledger_write(led: dict) -> None:
    tmp = LEDGER.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(led, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(LEDGER)


# ── core handler (network-free, unit-testable) ──────────────────────────────

def fulfil(priv, cfg: dict, order: dict) -> dict:
    """Issue + deliver for one parsed order. Idempotent via the ledger.
    Returns a small status dict. Never raises for a normal failure — it spools.

    The whole read-modify-write runs under one lock: ThreadingHTTPServer handles
    requests concurrently, and two racing orders (or an order racing its own LS
    retry) would otherwise clobber each other's ledger entries."""
    with _LEDGER_LOCK:
        led = ledger_read()
        prior = led.get(order["order_id"])
        if prior and prior.get("delivered"):
            return {"status": "duplicate", "order": order["order_id"]}

        spec = plan_for_order(cfg, order["variant_id"], order["variant_name"])
        key = prior.get("key") if prior else None
        if not key:
            key = build_key(
                priv,
                email=order["email"],
                plan=spec["plan"],
                seats=spec["seats"],
                order=str(order["order_id"]),
                expires=spec["expires"],
            )

        body = render_email(key, spec["seats"], spec["plan"], spec["expires"])
        delivered = deliver_email(order["email"], body)
        if not delivered:
            spool(order["order_id"], order["email"], key, reason="smtp-unconfigured-or-failed")

        led[order["order_id"]] = {
            "email": order["email"],
            "plan": spec["plan"],
            "key": key,
            "delivered": delivered,
            "issued_at": int(time.time()),
        }
        ledger_write(led)
        print(f"[ISSUE] order {order['order_id']} -> {order['email']} "
              f"({spec['plan']}, {spec['seats']} seats), delivered={delivered}", flush=True)
        return {"status": "delivered" if delivered else "spooled", "order": order["order_id"]}


def verify_signature(secret: str, raw: bytes, header_sig: str | None) -> bool:
    if not secret or not header_sig:
        return False
    digest = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    # LS sends hex; compare constant-time.
    return hmac.compare_digest(digest, header_sig.strip())


# ── HTTP server ─────────────────────────────────────────────────────────────

def make_handler(priv, cfg: dict, secret: str):
    class Handler(BaseHTTPRequestHandler):
        def _send(self, code: int, obj: dict) -> None:
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):  # health check
            if self.path.rstrip("/") in ("/healthz", "/health", ""):
                self._send(200, {"ok": True, "service": "cammy-fulfilment"})
            else:
                self._send(404, {"error": "not found"})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            if length > MAX_BODY_BYTES:
                self._send(413, {"error": "payload too large"})
                return
            raw = self.rfile.read(length) if length else b""
            sig = self.headers.get("X-Signature")
            if not verify_signature(secret, raw, sig):
                print("[REJECT] bad or missing X-Signature", flush=True)
                self._send(401, {"error": "invalid signature"})
                return
            try:
                body = json.loads(raw)
            except json.JSONDecodeError:
                self._send(400, {"error": "invalid json"})
                return
            order = parse_order(body)
            if order is None:
                # Valid signature, but not an order we act on — ack so LS stops retrying.
                self._send(200, {"status": "ignored"})
                return
            try:
                result = fulfil(priv, cfg, order)
                self._send(200, result)
            except Exception as e:  # last-resort: never 500 a paid order into a retry storm without a trace
                print(f"[ERROR] fulfilment crashed for {order.get('order_id')}: {e}", flush=True)
                try:
                    spool(order["order_id"], order["email"], f"(unsigned — issuance crashed: {e})", reason="crash")
                except Exception:
                    pass
                self._send(200, {"status": "error-spooled"})

        def log_message(self, *a):  # quiet default access log; we log our own events
            pass

    return Handler


def serve(port: int) -> None:
    secret = os.environ.get("LEMON_SQUEEZY_WEBHOOK_SECRET", "")
    if not secret:
        sys.exit("set LEMON_SQUEEZY_WEBHOOK_SECRET (the webhook signing secret from Lemon Squeezy)")
    priv = load_seed(None)
    cfg = load_config()
    httpd = ThreadingHTTPServer(("0.0.0.0", port), make_handler(priv, cfg, secret))
    print(f"[START] Cammy fulfilment on :{port}  spool={SPOOL_DIR}  ledger={LEDGER}", flush=True)
    print(f"[START] SMTP {'configured' if os.environ.get('SMTP_HOST') else 'NOT configured — keys will spool'}", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n[STOP]", flush=True)


# ── self-test ───────────────────────────────────────────────────────────────

def selftest() -> None:
    import tempfile
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

    global SPOOL_DIR, LEDGER
    tmp = Path(tempfile.mkdtemp(prefix="cammy-fulfil-test-"))
    SPOOL_DIR = tmp / "spool"
    LEDGER = tmp / "ledger.json"

    priv = Ed25519PrivateKey.generate()
    pub = priv.public_key()
    secret = "whsec_test_1234"

    def verify_key(key: str) -> dict:
        assert key.startswith("CAMMY-")
        payload_b64, sig_b64 = key[len("CAMMY-"):].split(".", 1)
        def unb64(s): return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))
        pb, sig = unb64(payload_b64), unb64(sig_b64)
        pub.verify(sig, pb)  # raises on bad sig
        return json.loads(pb)

    webhook = {
        "meta": {"event_name": "order_created"},
        "data": {"id": "99001", "attributes": {
            "user_email": "buyer@example.com", "order_number": 1042, "status": "paid",
            "first_order_item": {"variant_id": 555, "variant_name": "Lifetime", "product_name": "Cammy"},
        }},
    }
    raw = json.dumps(webhook).encode()
    good_sig = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()

    # 1. signature verification
    assert verify_signature(secret, raw, good_sig), "good signature rejected"
    assert not verify_signature(secret, raw, "deadbeef"), "bad signature accepted"
    assert not verify_signature(secret, raw, None), "missing signature accepted"
    assert not verify_signature(secret, raw + b"x", good_sig), "tampered body accepted"

    # 2. parse + issue (SMTP unset → spool)
    cfg = {}
    order = parse_order(webhook)
    assert order and order["email"] == "buyer@example.com"
    r1 = fulfil(priv, cfg, order)
    assert r1["status"] == "spooled", r1
    led = ledger_read()
    key = led["99001"]["key"]
    payload = verify_key(key)
    assert payload["email"] == "buyer@example.com" and payload["plan"] == "lifetime", payload
    assert payload["order"] == "99001", payload
    spooled = list(SPOOL_DIR.glob("order-*.txt"))
    assert len(spooled) == 1, spooled

    # 3. idempotency — replay the same order, no re-issue
    r2 = fulfil(priv, cfg, order)
    assert r2["status"] in ("duplicate", "spooled"), r2
    assert ledger_read()["99001"]["key"] == key, "key changed on replay"

    # 4. subscription variant via config mapping → expiry present
    sub_hook = json.loads(raw)
    sub_hook["data"]["id"] = "99002"
    sub_hook["data"]["attributes"]["first_order_item"]["variant_id"] = 777
    sub_order = parse_order(sub_hook)
    sub_cfg = {"variants": {"777": {"plan": "subscription", "expires_in_days": 365}}}
    fulfil(priv, sub_cfg, sub_order)
    sub_payload = verify_key(ledger_read()["99002"]["key"])
    assert sub_payload["plan"] == "subscription" and sub_payload["expires"], sub_payload

    # 5. refunded order is ignored
    refund = json.loads(raw)
    refund["data"]["attributes"]["status"] = "refunded"
    assert parse_order(refund) is None, "refunded order was not ignored"

    # 6. a pending order is deferred; the order_updated that marks it paid fulfils
    pending = json.loads(raw)
    pending["data"]["id"] = "99003"
    pending["data"]["attributes"]["status"] = "pending"
    assert parse_order(pending) is None, "pending order was fulfilled early"
    updated = json.loads(raw)
    updated["data"]["id"] = "99003"
    updated["meta"]["event_name"] = "order_updated"
    updated["data"]["attributes"]["status"] = "paid"
    upd_order = parse_order(updated)
    assert upd_order and upd_order["order_id"] == "99003", "paid order_updated not fulfillable"
    fulfil(priv, cfg, upd_order)
    assert "99003" in ledger_read(), "order_updated order not issued"

    print("selftest OK — signature check, issuance, idempotency, subscription mapping, refund guard all pass")
    print(f"  sample key: {key[:48]}…")


def main() -> None:
    p = argparse.ArgumentParser(description="Cammy license fulfilment webhook server.")
    p.add_argument("--port", type=int, default=8787)
    p.add_argument("--selftest", action="store_true", help="run the in-process pipeline test and exit")
    args = p.parse_args()
    if args.selftest:
        selftest()
        return
    serve(args.port)


if __name__ == "__main__":
    main()
