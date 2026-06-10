// Payment configuration.
//
// To enable purchases (cards + Apple Pay + Google Pay):
//   1. Create a Stripe account and a Payment Link for the $49 product
//      (Dashboard -> Payment Links -> New). Apple Pay & Google Pay are
//      included automatically with Payment Links / Checkout.
//   2. Register the site domain under Settings -> Payment methods ->
//      Apple Pay so Apple Pay appears in Safari.
//   3. Paste the link below.
window.STRIPE_PAYMENT_LINK = "https://buy.stripe.com/REPLACE_ME";
