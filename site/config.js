// Payment configuration.
//
// Cammy is sold via Lemon Squeezy (merchant of record — they handle
// VAT/sales tax, cards and PayPal worldwide).
//
//   1. Create the $79 "Cammy" product in the Lemon Squeezy dashboard
//      (Store -> Products -> New Product, single payment, Publish).
//   2. Run `python3 scripts/ls_setup.py --check` to confirm it landed and
//      print the product's buy URL.
//   3. Paste that buy URL below. The in-app upgrade buttons use the same
//      URL via the CAMMY_BUY_URL env var on the server.
//
// While this is empty, the buy buttons fall back to the free-trial download.
window.CAMMY_CHECKOUT_URL = "";
