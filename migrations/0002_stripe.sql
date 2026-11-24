-- Memberships paid by Stripe, see src/stripe.rs. users.member_until follows
-- the subscription's paid period.
ALTER TABLE users ADD COLUMN stripe_customer_id TEXT;
ALTER TABLE users ADD COLUMN stripe_subscription_id TEXT;
CREATE UNIQUE INDEX users_stripe_customer_id ON users (stripe_customer_id);
