-- When each member last used the site, so an account nobody has used for two
-- years can be deleted, after an email warning a month before; see
-- `Accounts::to_warn` and `Accounts::to_delete` in src/accounts.rs.
ALTER TABLE users ADD COLUMN last_seen_at TEXT;
ALTER TABLE users ADD COLUMN inactivity_warned_at TEXT;
UPDATE users SET last_seen_at = created_at;
