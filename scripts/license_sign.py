#!/usr/bin/env python3
"""Cammy license key signer / keygen — the issuance side of crates/core/licensing.rs.

A license key is an Ed25519-signed token the app verifies **offline**:

    CAMMY-<base64url(payload_json)>.<base64url(signature)>

The app embeds only the PUBLIC key; this script (and only this script) holds the
private seed. Run it from your fulfilment step — a Lemon Squeezy "order created"
webhook, or by hand for a manual sale. The private seed must never ship in the
app or land in the repo.

Requires: `pip install cryptography`.

Generate a production keypair (do this once, keep the seed offline):

    python3 scripts/license_sign.py --gen-key
    # → paste PUBLIC into LICENSE_PUBKEY_B64URL in crates/core/src/licensing.rs
    # → store SEED in your secrets manager / env CAMMY_LICENSE_SEED

Sign a lifetime license:

    CAMMY_LICENSE_SEED=<hex> python3 scripts/license_sign.py \
        --email buyer@example.com --plan lifetime --seats 2 --order LS-12345

Sign a 1-year update-plan (subscription) key:

    CAMMY_LICENSE_SEED=<hex> python3 scripts/license_sign.py \
        --email buyer@example.com --plan subscription --seats 2 \
        --order LS-12346 --expires-in-days 365
"""
import argparse
import base64
import json
import os
import sys
import time

try:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
except ImportError:
    sys.exit("this script needs the 'cryptography' package: pip install cryptography")


def b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def gen_key() -> None:
    priv = Ed25519PrivateKey.generate()
    seed = priv.private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    pub = priv.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    print("PUBLIC  (embed as LICENSE_PUBKEY_B64URL):")
    print("  " + b64u(pub))
    print()
    print("SEED    (KEEP OFFLINE — this signs every license):")
    print("  " + seed.hex())


def load_seed(arg_seed: str | None) -> Ed25519PrivateKey:
    hexed = arg_seed or os.environ.get("CAMMY_LICENSE_SEED")
    if not hexed:
        sys.exit("no signing seed: pass --seed <hex> or set CAMMY_LICENSE_SEED")
    try:
        raw = bytes.fromhex(hexed.strip())
    except ValueError:
        sys.exit("seed must be hex (see --gen-key)")
    if len(raw) != 32:
        sys.exit(f"seed must be 32 bytes (64 hex chars), got {len(raw)}")
    return Ed25519PrivateKey.from_private_bytes(raw)


def sign(args: argparse.Namespace) -> None:
    priv = load_seed(args.seed)

    expires = None
    if args.expires_in_days is not None:
        expires = int(time.time()) + args.expires_in_days * 86400
    elif args.expires is not None:
        expires = args.expires

    if args.plan == "subscription" and expires is None:
        sys.exit("a 'subscription' plan needs --expires-in-days or --expires")

    payload = {
        "v": 1,
        "email": args.email,
        "plan": args.plan,
        "seats": args.seats,
        "order": args.order,
        "issued": int(time.time()),
        "expires": expires,
    }
    # Compact, sorted — must match what the verifier signs over byte-for-byte.
    pb = json.dumps(payload, separators=(",", ":"), sort_keys=True).encode()
    sig = priv.sign(pb)
    key = "CAMMY-" + b64u(pb) + "." + b64u(sig)
    if args.quiet:
        print(key)
    else:
        print("payload:", pb.decode())
        print("key:")
        print(key)


def main() -> None:
    p = argparse.ArgumentParser(description="Sign / generate Cammy license keys.")
    p.add_argument("--gen-key", action="store_true", help="generate a new keypair and exit")
    p.add_argument("--email", help="buyer email")
    p.add_argument(
        "--plan",
        choices=["lifetime", "subscription"],
        default="lifetime",
        help="perpetual license, or a time-bounded update plan",
    )
    p.add_argument("--seats", type=int, default=2, help="allowed activations")
    p.add_argument("--order", default="", help="merchant order id (for support)")
    p.add_argument("--expires-in-days", type=int, help="subscription window from now")
    p.add_argument("--expires", type=int, help="explicit expiry (unix secs)")
    p.add_argument("--seed", help="signing seed hex (else $CAMMY_LICENSE_SEED)")
    p.add_argument("--quiet", action="store_true", help="print only the key")
    args = p.parse_args()

    if args.gen_key:
        gen_key()
        return
    if not args.email:
        p.error("--email is required to sign a license (or use --gen-key)")
    sign(args)


if __name__ == "__main__":
    main()
