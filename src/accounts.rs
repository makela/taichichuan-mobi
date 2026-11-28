//! Members' accounts in the site's database (see `db.rs`): logging in by a
//! link sent by email, the sessions of logged in browsers, and the lessons
//! each member has passed.
//!
//! The secrets in links and cookies are only stored as their SHA-256, so the
//! database alone doesn't let anyone log in.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::HashSet;
use std::fmt::Write;

use crate::Language;

/// How long an emailed link works.
pub const LINK_LIFETIME: Duration = Duration::minutes(30);
/// Links at most this often to one address, so the forms can't be used to
/// flood someone's inbox.
const LINK_INTERVAL: Duration = Duration::minutes(1);
/// How long a browser stays logged in.
pub const SESSION_LIFETIME: Duration = Duration::days(30);
/// An account nobody has used for this long is deleted, unless a membership
/// is paid: no need to keep the data any longer (the privacy notice says so).
pub const INACTIVE_AFTER: Duration = Duration::days(730);
/// The warning email goes this long before.
pub const WARNING_AHEAD: Duration = Duration::days(30);
/// When the site was last used is written at most this often, not on every
/// page.
const SEEN_EVERY: Duration = Duration::days(1);

pub type Result<T> = std::result::Result<T, sqlx::Error>;

/// A time as stored: UTC to the second, as text that sorts by time.
#[must_use]
pub fn timestamp(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A new secret for a link or a cookie.
fn new_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// An email address as typed, trimmed, if it is one.
#[must_use]
pub fn parse_email(text: &str) -> Option<String> {
    let email = text.trim();
    (email.len() <= 254 && email.parse::<lettre::Address>().is_ok()).then(|| email.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub language: Language,
    /// Until when the membership is paid, as stored.
    pub member_until: Option<String>,
    /// The Stripe customer of their membership payments, if they have paid.
    pub stripe_customer_id: Option<String>,
}

impl User {
    #[must_use]
    pub fn is_member(&self, now: DateTime<Utc>) -> bool {
        self.member_until
            .as_ref()
            .is_some_and(|until| *until > timestamp(now))
    }
}

fn user_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<User> {
    let language: String = row.try_get("language")?;
    Ok(User {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        language: Language::from_code(&language).unwrap_or(Language::En),
        member_until: row.try_get("member_until")?,
        stripe_customer_id: row.try_get("stripe_customer_id")?,
    })
}

/// What an emailed link is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkKind {
    /// Only sent to addresses with an account.
    Login,
    /// Sent to any address; the account is made when the link is used, and
    /// to an address with an account, it logs in.
    Register,
}

#[derive(Clone)]
pub struct Accounts {
    pool: SqlitePool,
}

impl Accounts {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// A new link for `email`, as the secret to put in it. None when no email
    /// should go out: a login for an address without an account, or another
    /// link sent to it moments ago. The page says the same either way, so it
    /// doesn't tell who has an account.
    pub async fn new_link(
        &self,
        email: &str,
        language: Language,
        kind: LinkKind,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        sqlx::query("DELETE FROM login_links WHERE expires_at <= ?")
            .bind(timestamp(now))
            .execute(&self.pool)
            .await?;
        if kind == LinkKind::Login && self.user_by_email(email).await?.is_none() {
            return Ok(None);
        }
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM login_links WHERE email = ? AND created_at > ?",
        )
        .bind(email)
        .bind(timestamp(now - LINK_INTERVAL))
        .fetch_one(&self.pool)
        .await?;
        if recent > 0 {
            return Ok(None);
        }
        let token = new_token()?;
        sqlx::query(
            "INSERT INTO login_links (token_hash, email, language, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(hash(&token))
        .bind(email)
        .bind(language.code())
        .bind(timestamp(now))
        .bind(timestamp(now + LINK_LIFETIME))
        .execute(&self.pool)
        .await?;
        Ok(Some(token))
    }

    /// Forgets a link whose email could not be sent, so another can be asked
    /// for at once.
    pub async fn forget_link(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM login_links WHERE token_hash = ?")
            .bind(hash(token))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Uses up the link with `token`, making the account if there is none.
    /// None if the link is unknown, used or expired.
    pub async fn use_link(&self, token: &str, now: DateTime<Utc>) -> Result<Option<User>> {
        let mut transaction = self.pool.begin().await?;
        let Some(row) = sqlx::query(
            "DELETE FROM login_links WHERE token_hash = ? AND expires_at > ?
             RETURNING email, language",
        )
        .bind(hash(token))
        .bind(timestamp(now))
        .fetch_optional(&mut *transaction)
        .await?
        else {
            return Ok(None);
        };
        let email: String = row.try_get("email")?;
        let language: String = row.try_get("language")?;
        sqlx::query(
            "INSERT INTO users (email, language, created_at) VALUES (?, ?, ?)
             ON CONFLICT (email) DO NOTHING",
        )
        .bind(&email)
        .bind(&language)
        .bind(timestamp(now))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE users SET last_seen_at = ?, inactivity_warned_at = NULL WHERE email = ?",
        )
        .bind(timestamp(now))
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
        let user = sqlx::query("SELECT * FROM users WHERE email = ?")
            .bind(&email)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        user_from_row(&user).map(Some)
    }

    pub async fn user_by_email(&self, email: &str) -> Result<Option<User>> {
        sqlx::query("SELECT * FROM users WHERE email = ?")
            .bind(email)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(user_from_row)
            .transpose()
    }

    /// Logs a browser in as `user_id`, returning the secret for its cookie.
    pub async fn new_session(&self, user_id: i64, now: DateTime<Utc>) -> Result<String> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(timestamp(now))
            .execute(&self.pool)
            .await?;
        let token = new_token()?;
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(hash(&token))
        .bind(user_id)
        .bind(timestamp(now))
        .bind(timestamp(now + SESSION_LIFETIME))
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    /// Who is logged in with the cookie secret `token`, if anyone. Using the
    /// site counts as being seen, and lifts a warning that the account is to
    /// be deleted.
    pub async fn session_user(&self, token: &str, now: DateTime<Utc>) -> Result<Option<User>> {
        let user = sqlx::query(
            "SELECT users.* FROM sessions JOIN users ON users.id = sessions.user_id
             WHERE sessions.token_hash = ? AND sessions.expires_at > ?",
        )
        .bind(hash(token))
        .bind(timestamp(now))
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(user_from_row)
        .transpose()?;
        if let Some(user) = &user {
            sqlx::query(
                "UPDATE users SET last_seen_at = ?, inactivity_warned_at = NULL
                 WHERE id = ? AND (last_seen_at IS NULL OR last_seen_at < ?)",
            )
            .bind(timestamp(now))
            .bind(user.id)
            .bind(timestamp(now - SEEN_EVERY))
            .execute(&self.pool)
            .await?;
        }
        Ok(user)
    }

    /// The accounts to warn now that they'll be deleted: unused for nearly
    /// [`INACTIVE_AFTER`], not warned yet, and without a paid membership.
    pub async fn to_warn(&self, now: DateTime<Utc>) -> Result<Vec<User>> {
        sqlx::query(
            "SELECT * FROM users WHERE last_seen_at < ? AND inactivity_warned_at IS NULL
             AND (member_until IS NULL OR member_until <= ?)",
        )
        .bind(timestamp(now - INACTIVE_AFTER + WARNING_AHEAD))
        .bind(timestamp(now))
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(user_from_row)
        .collect()
    }

    pub async fn mark_warned(&self, user_id: i64, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE users SET inactivity_warned_at = ? WHERE id = ?")
            .bind(timestamp(now))
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The accounts to delete now: unused for [`INACTIVE_AFTER`], warned at
    /// least [`WARNING_AHEAD`] ago (so a warning sent late still gives its
    /// full month), and without a paid membership.
    pub async fn to_delete(&self, now: DateTime<Utc>) -> Result<Vec<User>> {
        sqlx::query(
            "SELECT * FROM users WHERE last_seen_at < ? AND inactivity_warned_at <= ?
             AND (member_until IS NULL OR member_until <= ?)",
        )
        .bind(timestamp(now - INACTIVE_AFTER))
        .bind(timestamp(now - WARNING_AHEAD))
        .bind(timestamp(now))
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(user_from_row)
        .collect()
    }

    /// For tests: as if `user_id` was last seen at `time`.
    #[cfg(test)]
    pub async fn set_last_seen(&self, user_id: i64, time: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE users SET last_seen_at = ? WHERE id = ?")
            .bind(timestamp(time))
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn end_session(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(hash(token))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Deletes the account with its sessions and progress.
    pub async fn delete_user(&self, user_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn user_by_id(&self, id: i64) -> Result<Option<User>> {
        sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(user_from_row)
            .transpose()
    }

    pub async fn user_by_stripe_customer(&self, customer: &str) -> Result<Option<User>> {
        sqlx::query("SELECT * FROM users WHERE stripe_customer_id = ?")
            .bind(customer)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(user_from_row)
            .transpose()
    }

    /// Records the user's Stripe subscription: an active one makes them a
    /// member `until`, and an ended one ends the membership if it was the
    /// subscription that paid for it.
    pub async fn record_subscription(
        &self,
        user_id: i64,
        customer: &str,
        subscription: &str,
        until: Option<DateTime<Utc>>,
    ) -> Result<()> {
        match until {
            Some(until) => {
                sqlx::query(
                    "UPDATE users SET member_until = ?, stripe_customer_id = ?,
                     stripe_subscription_id = ? WHERE id = ?",
                )
                .bind(timestamp(until))
                .bind(customer)
                .bind(subscription)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE users SET member_until = NULL, stripe_subscription_id = NULL
                     WHERE id = ? AND stripe_subscription_id = ?",
                )
                .bind(user_id)
                .bind(subscription)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    /// Sets until when the membership is paid, or None to end it.
    pub async fn set_member_until(&self, user_id: i64, until: Option<DateTime<Utc>>) -> Result<()> {
        sqlx::query("UPDATE users SET member_until = ? WHERE id = ?")
            .bind(until.map(timestamp))
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Everything stored about `user_id`, for them to download (their right
    /// of access and to data portability): the account, the membership,
    /// the lessons passed and the browsers logged in. None if there's no
    /// such account. Times are UTC, as stored.
    pub async fn personal_data(&self, user_id: i64) -> Result<Option<serde_json::Value>> {
        let Some(user) = sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let lessons: Vec<(String, String)> = sqlx::query_as(
            "SELECT slug, completed_at FROM completed_lessons WHERE user_id = ?
             ORDER BY completed_at, slug",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        let logins: Vec<(String, String)> = sqlx::query_as(
            "SELECT created_at, expires_at FROM sessions WHERE user_id = ? ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(Some(serde_json::json!({
            "account": {
                "email": user.try_get::<String, _>("email")?,
                "language": user.try_get::<String, _>("language")?,
                "registered": user.try_get::<String, _>("created_at")?,
                "last_seen": user.try_get::<Option<String>, _>("last_seen_at")?,
                "warned_of_deletion": user.try_get::<Option<String>, _>("inactivity_warned_at")?,
            },
            "membership": {
                "paid_until": user.try_get::<Option<String>, _>("member_until")?,
                "stripe_customer": user.try_get::<Option<String>, _>("stripe_customer_id")?,
                "stripe_subscription": user.try_get::<Option<String>, _>("stripe_subscription_id")?,
            },
            "lessons_passed": lessons
                .into_iter()
                .map(|(lesson, passed)| serde_json::json!({ "lesson": lesson, "passed": passed }))
                .collect::<Vec<_>>(),
            "logged_in_browsers": logins
                .into_iter()
                .map(|(since, until)| serde_json::json!({ "since": since, "until": until }))
                .collect::<Vec<_>>(),
        })))
    }

    /// The slugs of the lessons `user_id` has passed.
    pub async fn completed(&self, user_id: i64) -> Result<HashSet<String>> {
        let slugs: Vec<String> =
            sqlx::query_scalar("SELECT slug FROM completed_lessons WHERE user_id = ?")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(slugs.into_iter().collect())
    }

    pub async fn complete(&self, user_id: i64, slug: &str, now: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO completed_lessons (user_id, slug, completed_at) VALUES (?, ?, ?)
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(slug)
        .bind(timestamp(now))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn time(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, hour, minute, 0)
            .single()
            .unwrap_or_default()
    }

    async fn accounts() -> Result<Accounts> {
        Ok(Accounts::new(crate::db::in_memory().await?))
    }

    #[test]
    fn parses_email_addresses() {
        assert_eq!(parse_email(" a@b.fi "), Some("a@b.fi".to_string()));
        assert_eq!(parse_email("a@"), None);
        assert_eq!(parse_email(""), None);
        assert_eq!(parse_email(&format!("{}@b.fi", "a".repeat(260))), None);
    }

    #[tokio::test]
    async fn registering_makes_the_account_when_the_link_is_used() -> TestResult {
        let accounts = accounts().await?;
        let now = time(12, 0);
        let token = accounts
            .new_link("Ann@Example.fi", Language::Fi, LinkKind::Register, now)
            .await?
            .ok_or("no link")?;
        assert_eq!(token.len(), 64);
        assert!(accounts.user_by_email("ann@example.fi").await?.is_none());

        let user = accounts
            .use_link(&token, now)
            .await?
            .ok_or("link refused")?;
        assert_eq!(user.email, "Ann@Example.fi");
        assert_eq!(user.language, Language::Fi);
        assert!(!user.is_member(now));
        // Each link works once.
        assert!(accounts.use_link(&token, now).await?.is_none());
        // The same address in other case is the same account.
        assert_eq!(
            accounts
                .user_by_email("ann@example.fi")
                .await?
                .map(|user| user.id),
            Some(user.id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn login_links_only_go_to_accounts() -> TestResult {
        let accounts = accounts().await?;
        let now = time(12, 0);
        assert!(
            accounts
                .new_link("ann@example.fi", Language::En, LinkKind::Login, now)
                .await?
                .is_none()
        );
        let token = accounts
            .new_link("ann@example.fi", Language::En, LinkKind::Register, now)
            .await?
            .ok_or("no link")?;
        accounts.use_link(&token, now).await?;
        let later = time(12, 5);
        let token = accounts
            .new_link("ANN@example.fi", Language::En, LinkKind::Login, later)
            .await?
            .ok_or("no login link")?;
        // Registering again just logs in to the same account.
        let user = accounts.use_link(&token, later).await?.ok_or("refused")?;
        assert_eq!(user.email, "ann@example.fi");
        Ok(())
    }

    #[tokio::test]
    async fn links_expire_and_are_not_sent_too_often() -> TestResult {
        let accounts = accounts().await?;
        let token = accounts
            .new_link("a@b.fi", Language::En, LinkKind::Register, time(12, 0))
            .await?
            .ok_or("no link")?;
        assert!(
            accounts
                .new_link("a@b.fi", Language::En, LinkKind::Register, time(12, 0))
                .await?
                .is_none(),
            "a second link at once"
        );
        let second = accounts
            .new_link("a@b.fi", Language::En, LinkKind::Register, time(12, 2))
            .await?
            .ok_or("no second link")?;
        assert!(
            accounts.use_link(&token, time(12, 30)).await?.is_none(),
            "expired"
        );
        assert!(accounts.use_link(&second, time(12, 31)).await?.is_some());

        let failed = accounts
            .new_link("c@d.fi", Language::En, LinkKind::Register, time(13, 0))
            .await?
            .ok_or("no link")?;
        accounts.forget_link(&failed).await?;
        assert!(accounts.use_link(&failed, time(13, 0)).await?.is_none());
        assert!(
            accounts
                .new_link("c@d.fi", Language::En, LinkKind::Register, time(13, 0))
                .await?
                .is_some(),
            "a new link at once after a failed email"
        );
        Ok(())
    }

    async fn new_user(
        accounts: &Accounts,
        email: &str,
    ) -> std::result::Result<User, Box<dyn std::error::Error>> {
        let token = accounts
            .new_link(email, Language::En, LinkKind::Register, time(12, 0))
            .await?
            .ok_or("no link")?;
        Ok(accounts
            .use_link(&token, time(12, 0))
            .await?
            .ok_or("refused")?)
    }

    #[tokio::test]
    async fn sessions_log_in_until_they_end_or_expire() -> TestResult {
        let accounts = accounts().await?;
        let user = new_user(&accounts, "a@b.fi").await?;
        let token = accounts.new_session(user.id, time(12, 0)).await?;
        assert_eq!(
            accounts
                .session_user(&token, time(13, 0))
                .await?
                .map(|user| user.id),
            Some(user.id)
        );
        assert!(accounts.session_user("wrong", time(13, 0)).await?.is_none());
        let expired = time(12, 0) + SESSION_LIFETIME;
        assert!(accounts.session_user(&token, expired).await?.is_none());

        let token = accounts.new_session(user.id, time(12, 0)).await?;
        accounts.end_session(&token).await?;
        assert!(accounts.session_user(&token, time(12, 1)).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn membership_lasts_until_its_end() -> TestResult {
        let accounts = accounts().await?;
        let user = new_user(&accounts, "a@b.fi").await?;
        accounts
            .set_member_until(user.id, Some(time(18, 0)))
            .await?;
        let user = accounts.user_by_email("a@b.fi").await?.ok_or("gone")?;
        assert!(user.is_member(time(17, 59)));
        assert!(!user.is_member(time(18, 0)));
        Ok(())
    }

    #[tokio::test]
    async fn subscriptions_set_the_membership_until_they_end() -> TestResult {
        let accounts = accounts().await?;
        let user = new_user(&accounts, "a@b.fi").await?;
        accounts
            .record_subscription(user.id, "cus_1", "sub_1", Some(time(18, 0)))
            .await?;
        let found = accounts
            .user_by_stripe_customer("cus_1")
            .await?
            .ok_or("no customer")?;
        assert_eq!(found.id, user.id);
        assert!(found.is_member(time(17, 0)));

        // An old subscription ending leaves the current one alone.
        accounts
            .record_subscription(user.id, "cus_1", "sub_0", None)
            .await?;
        assert!(
            accounts
                .user_by_id(user.id)
                .await?
                .ok_or("gone")?
                .is_member(time(17, 0))
        );
        accounts
            .record_subscription(user.id, "cus_1", "sub_1", None)
            .await?;
        let ended = accounts.user_by_id(user.id).await?.ok_or("gone")?;
        assert!(!ended.is_member(time(17, 0)));
        // The customer is kept, for receipts and joining again.
        assert_eq!(ended.stripe_customer_id.as_deref(), Some("cus_1"));
        Ok(())
    }

    #[tokio::test]
    async fn deleting_an_account_removes_its_sessions_and_progress() -> TestResult {
        let accounts = accounts().await?;
        let user = new_user(&accounts, "a@b.fi").await?;
        let other = new_user(&accounts, "c@d.fi").await?;
        accounts.complete(user.id, "opening", time(12, 0)).await?;
        accounts.complete(user.id, "opening", time(12, 1)).await?;
        accounts
            .complete(other.id, "preparation", time(12, 0))
            .await?;
        assert_eq!(
            accounts.completed(user.id).await?,
            HashSet::from(["opening".to_string()])
        );
        let session = accounts.new_session(user.id, time(12, 0)).await?;

        accounts.delete_user(user.id).await?;
        assert!(accounts.user_by_email("a@b.fi").await?.is_none());
        assert!(
            accounts
                .session_user(&session, time(12, 1))
                .await?
                .is_none()
        );
        assert!(accounts.completed(user.id).await?.is_empty());
        assert_eq!(accounts.completed(other.id).await?.len(), 1);
        Ok(())
    }
}
