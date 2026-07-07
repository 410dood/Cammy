# 09 — Licensing & Monetization

Implements Launch Roadmap **Phase 1 ("Sellable Product")**: the trial + license
layer that turns Cammy from a repo into something you can charge for. The
go-to-market strategy (pricing, positioning, channels) lives in `business/`; this
document is the *engineering* companion — how the entitlement system works and
how to operate it.

## Model (from the Business Plan)

- **$79 one-time**, unlimited cameras, perpetual license.
- Optional **$29/yr update plan** (a `subscription` key with an `expires`).
- **30-day full-featured trial**, no feature locks during the trial.
- Sold via **Lemon Squeezy** (merchant of record — handles VAT/tax).

## Design principle: never brick a camera system

The single hard rule (Business Plan, Launch Roadmap, and `PRODUCT.md` all agree):
a surveillance system must not stop protecting a home because a license lapsed or
a server was unreachable. So:

- **Verification is fully offline.** Keys are Ed25519-signed tokens checked
  against an embedded public key. No license server is ever contacted.
- **Expiry never disables recording or viewing.** An expired trial still runs;
  it only surfaces an upgrade nudge. A lapsed *update plan* still runs — its
  `expires` bounds access to newer builds, not usage.
- **The UI does not nag.** Per `PRODUCT.md` anti-references (no upsell banners,
  no paywall badges), the shell banner is hidden while licensed and for the
  first ~23 days of trial; it appears only in the last week and after expiry.

Enforcement policy (what, if anything, to restrict after trial) is intentionally
*not* wired into request handling. `Entitlement::allows_config()` in
`crates/core/src/licensing.rs` is the single seam to change if you later decide
to, e.g., make config read-only post-trial. It ships mirroring `is_active()`
(no restriction) — a deliberate business decision left to you.

## Key format

```
CAMMY-<base64url(payload_json)>.<base64url(ed25519_signature)>
```

The signature is Ed25519 over the exact payload bytes. Payload (schema `v: 1`):

```json
{"v":1,"email":"buyer@example.com","plan":"lifetime","seats":2,
 "order":"LS-12345","issued":1749686400,"expires":null}
```

- `plan`: `"lifetime"` (perpetual, `expires: null`) or `"subscription"`
  (update-plan window, `expires` = unix secs).
- `seats`: allowed activations (informational in-app; enforced at issuance).
- The verifier checks the signature over the decoded payload bytes, then parses
  JSON — so there is no canonicalization dependency between signer and verifier.

## Components

| Piece | Where |
|---|---|
| Entitlement engine (verify, trial clock, status) | `crates/core/src/licensing.rs` |
| API: `GET/POST/DELETE /api/license` | `crates/core/src/api.rs` |
| Admin gate on POST/DELETE | `crates/core/src/auth.rs` (`required_role`) |
| Trial started at boot | `crates/core/src/lib.rs` (`ensure_trial_started`) |
| Shell banner + Settings pane | `web/src/License.tsx`, wired in `App.tsx` / `pages/Settings.tsx` |
| Key signer / keygen (issuance) | `scripts/license_sign.py` |

## Operating it

### 1. Generate your production keypair (once)

```
python3 scripts/license_sign.py --gen-key
```

- Paste the **PUBLIC** key into `LICENSE_PUBKEY_B64URL` in
  `crates/core/src/licensing.rs` and rebuild.
- Store the **SEED** offline (secrets manager / `CAMMY_LICENSE_SEED`). It signs
  every license; if it leaks, rotate the embedded public key (which invalidates
  all keys signed by the old seed) and re-issue.

> The value currently embedded is a **development** key. The unit tests in
> `licensing.rs` and the sample keys pinned there are signed by the matching dev
> seed (reproducible via the script). Regenerate both when you rotate.

### 2. Issue a license

Manual sale:

```
CAMMY_LICENSE_SEED=<hex> python3 scripts/license_sign.py \
  --email buyer@example.com --plan lifetime --seats 2 --order LS-12345
```

Automated (Lemon Squeezy `order_created` webhook): run `scripts/fulfilment_server.py`.
It verifies the webhook HMAC, signs a key with the same `build_key` logic
`license_sign.py` uses (one source of truth for the format), and emails it to the
buyer. The seed lives only in that server's environment.

```
export CAMMY_LICENSE_SEED=<hex>
export LEMON_SQUEEZY_WEBHOOK_SECRET=<the webhook signing secret>
export SMTP_HOST=... SMTP_PORT=587 SMTP_USER=... SMTP_PASS=... \
       SMTP_FROM='Cammy <licenses@cammy.app>'
python3 scripts/fulfilment_server.py --port 8787   # put behind TLS
```

Then in Lemon Squeezy → Settings → Webhooks, add a callback URL pointing at this
server for the **`order_created`** event, using the same signing secret.

Operational guarantees, so a paid order is never lost:

- **Idempotent.** A JSON ledger (`fulfilment-ledger.json`) keys issuance by order
  id; Lemon Squeezy retries webhooks, and a retry reuses the same key rather than
  minting a second one.
- **Spool fallback.** If SMTP is unconfigured or a send fails, the key is written
  to `fulfilment-spool/order-<id>.txt` and logged loudly — deliver it by hand and
  nothing is dropped.
- **Signed-only.** Requests without a valid `X-Signature` HMAC are rejected 401
  before any work happens; refunded/failed orders are ignored.
- **Variant mapping.** Every order issues a lifetime 2-seat key by default. To
  sell the update plan as a separate variant, set `CAMMY_FULFILMENT_CONFIG` to a
  JSON file mapping Lemon Squeezy variant ids to `{"plan": "subscription",
  "expires_in_days": 365}` (see the server's module docstring).

Validate the whole pipeline offline before going live:

```
python3 scripts/fulfilment_server.py --selftest
```

### 3. Customer activates

Settings → **License** → paste the `CAMMY-…` key → Activate. Verification is
local; works air-gapped. "Remove license" frees the machine for a move.

## Testing

`crates/core/src/licensing.rs` ships unit tests (run `cargo test -p zoomy`):
genuine-key verification, prefix tolerance, tamper rejection, garbage rejection,
subscription expiry parsing, trial start/report, activate→license→deactivate
round-trip, and trial-tamper fail-safe. The web layer typechecks under
`cd web && npx tsc --noEmit`.

## Trial integrity (honest limits)

First run stamps `license.trial_start` with an HMAC tag; a hand-edited timestamp
is detected and fails safe to "expired". This is **deterrence, not DRM** — an
offline product cannot be made uncrackable, and the embedded HMAC secret is
extractable by definition. Deleting the DB restarts the trial; that is an
accepted trade-off of the local-first, no-phone-home design. The paying customer
gets the frictionless path; that is the whole strategy.

## Not yet done (future layers, per roadmap)

- **Online seat activation** against Lemon Squeezy's license API (the 2-machine
  limit is currently issuance-side only; in-app it is informational).
- **Auto-update channel** honoring a subscription's `expires` window.
- **Cloud-relay subscription** (~$4–5/mo remote access) — the Year-2 recurring
  layer; a separate service, not part of this module.
