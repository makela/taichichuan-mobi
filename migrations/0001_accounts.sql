-- Members' accounts, see src/accounts.rs. Times are UTC, as text like
-- 2026-11-24T12:00:00Z, so they sort and compare as text.
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- en or fi: the language of their emails.
    language TEXT NOT NULL,
    created_at TEXT NOT NULL,
    -- Until when the membership is paid, or NULL without one.
    member_until TEXT
);

-- Links emailed to log in or to register, by the SHA-256 of the secret in
-- the link. Each works once, until expires_at. The user is created when a
-- registration link is used.
CREATE TABLE login_links (
    token_hash TEXT PRIMARY KEY,
    email TEXT NOT NULL COLLATE NOCASE,
    language TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX login_links_email ON login_links (email, created_at);

-- Logged in browsers, by the SHA-256 of the secret in their cookie.
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX sessions_user_id ON sessions (user_id);

-- Lessons each member has passed, by the lesson's slug.
CREATE TABLE completed_lessons (
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    PRIMARY KEY (user_id, slug)
);
