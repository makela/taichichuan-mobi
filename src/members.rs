//! The members' area: registering and logging in with links sent by email,
//! the account page, and the lessons with each member's progress.
//!
//! The login and registration forms are made by `email_form`, so that they
//! can go on the front page too once the lessons are open to all.

use axum::{
    Form, Router,
    extract::{Path as UrlPath, Query, Request, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{Duration, Utc};
use lettre::message::Mailbox;
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::accounts::{self, Accounts, LinkKind, SESSION_LIFETIME, User};
use crate::lessons::{Access, Course, Kind, Learner, Lesson, LessonId, Passing, Set};
use crate::mail::{self, Encryption, Mailer};
use crate::{Language, LanguageQuery, Translations, layout, linked_markup, not_found};

const SESSION_COOKIE: &str = "session";

/// Shown in development under quiz and review lessons, which can be marked
/// practised there until quizzes and video reviews are made.
const DEV_PASSING_NOTE: &str = "Development only: quizzes and video reviews aren't made yet, so this lesson can be marked practised.";

pub struct Members {
    course: Course,
    accounts: Accounts,
    mailer: Mailer,
    from: Mailbox,
    /// Public address of the site without a trailing slash, for links in emails.
    site_url: String,
    /// Development mode: cookies also over plain HTTP, and a button on the
    /// account page to start and end a membership.
    dev: bool,
}

impl Members {
    /// The lessons from `content_dir`, with the database in `DATABASE_URL`
    /// and the emails sent as set by `MAIL_FROM` and the `SMTP_` variables;
    /// without `SMTP_HOST` the emails are only logged.
    pub async fn from_env(content_dir: &Path, site_url: &str, dev: bool) -> Result<Self, String> {
        let course = Course::load(content_dir)?;
        tracing::info!(
            content_dir = %content_dir.display(),
            sets = course.sets().len(),
            lessons = course.sets().iter().map(|set| set.lessons.len()).sum::<usize>(),
            "Loaded the course"
        );
        let setting = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        let database_url = setting("DATABASE_URL").unwrap_or(crate::db::DEFAULT_URL.to_string());
        let pool = crate::db::open(&database_url)
            .await
            .map_err(|error| format!("database {database_url}: {error}"))?;
        let from: Mailbox = setting("MAIL_FROM")
            .unwrap_or(mail::DEFAULT_FROM.to_string())
            .parse()
            .map_err(|error| format!("MAIL_FROM isn't an address: {error}"))?;
        let mailer = match setting("SMTP_HOST") {
            None => {
                tracing::info!("SMTP_HOST isn't set: emails are only logged");
                Mailer::Log
            }
            Some(host) => {
                let encryption =
                    Encryption::parse(&setting("SMTP_ENCRYPTION").unwrap_or("tls".into()))?;
                let port = match setting("SMTP_PORT") {
                    Some(port) => port
                        .parse()
                        .map_err(|_| format!("SMTP_PORT isn't a port: {port}"))?,
                    None if encryption == Encryption::Tls => 465,
                    None => 587,
                };
                let login = match (setting("SMTP_USER"), setting("SMTP_PASSWORD_FILE")) {
                    (None, _) => None,
                    (Some(_), None) => return Err("SMTP_USER needs SMTP_PASSWORD_FILE".into()),
                    (Some(user), Some(file)) => {
                        let password = std::fs::read_to_string(&file)
                            .map_err(|error| format!("SMTP_PASSWORD_FILE {file}: {error}"))?;
                        Some((user, password.trim().to_string()))
                    }
                };
                tracing::info!(%host, port, ?encryption, "Sending emails by SMTP");
                Mailer::smtp(&host, port, encryption, login).map_err(|error| error.to_string())?
            }
        };
        Ok(Self {
            course,
            accounts: Accounts::new(pool),
            mailer,
            from,
            site_url: site_url.trim_end_matches('/').to_string(),
            dev,
        })
    }
}

pub fn router(members: Arc<Members>) -> Router {
    let router = Router::new()
        .route("/login", get(login_page).post(login))
        .route("/register", get(register_page).post(register))
        .route("/login/confirm", get(confirm_page).post(confirm))
        .route("/logout", post(logout))
        .route("/account", get(account_page))
        .route("/account/delete", post(delete_account))
        .route("/lessons", get(course_page))
        .route("/lessons/{set}", get(set_page))
        .route("/lessons/{set}/{lesson}", get(lesson_page))
        .route("/lessons/{set}/{lesson}/complete", post(complete_lesson))
        .route("/lessons/{set}/{lesson}/video", get(lesson_video));
    let router = if members.dev {
        router.route("/account/dev-membership", post(dev_membership))
    } else {
        router
    };
    router.with_state(members)
}

/// A failure, such as of the database, shown as a server error.
struct Failure(String);

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        Self(error.to_string())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "Members' page failed");
        let en = Translations::for_language(Language::En);
        let fi = Translations::for_language(Language::Fi);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            page_headers(),
            layout(
                Language::En,
                &en,
                "/",
                en.server_error_title,
                &html! {},
                &html! {
                    h2 { (en.server_error_title) }
                    p { (en.server_error) }
                    p lang="fi" { (fi.server_error_title) ". " (fi.server_error) }
                },
            ),
        )
            .into_response()
    }
}

type Page = Result<Response, Failure>;

/// Headers for the members' pages: browsers and proxies don't store them,
/// and search engines don't index them.
fn page_headers() -> [(header::HeaderName, HeaderValue); 3] {
    [
        (header::VARY, HeaderValue::from_static("Accept-Language")),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        ),
        (
            header::HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("noindex"),
        ),
    ]
}

/// The language and who is logged in, for a request.
struct Visit {
    language: Language,
    t: Translations,
    user: Option<User>,
    /// The secret in the session cookie, if one was sent.
    session: Option<String>,
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|cookie| {
            let (name, value) = cookie.trim().split_once('=')?;
            (name == SESSION_COOKIE && !value.is_empty()).then(|| value.to_string())
        })
}

impl Members {
    async fn visit(
        &self,
        query: Result<Query<LanguageQuery>, QueryRejection>,
        headers: &HeaderMap,
    ) -> Result<Visit, Failure> {
        let language = Language::from_request(&LanguageQuery::or_default(query), headers);
        let session = session_token(headers);
        let user = match &session {
            Some(token) => self.accounts.session_user(token, Utc::now()).await?,
            None => None,
        };
        Ok(Visit {
            language,
            t: Translations::for_language(language),
            user,
            session,
        })
    }

    fn session_cookie(&self, token: &str) -> String {
        format!(
            "{SESSION_COOKIE}={token}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{}",
            SESSION_LIFETIME.num_seconds(),
            if self.dev { "" } else { "; Secure" }
        )
    }

    fn removed_cookie(&self) -> String {
        format!(
            "{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{}",
            if self.dev { "" } else { "; Secure" }
        )
    }
}

/// A path in the visitor's language, e.g. `/login?lang=fi`.
fn href(path: &str, language: Language) -> String {
    format!("{path}?lang={}", language.code())
}

fn redirect(path: &str, language: Language) -> Response {
    Redirect::to(&href(path, language)).into_response()
}

/// A members' page: links to the other members' pages above `content`.
fn page(visit: &Visit, path: &str, title: &str, content: &Markup) -> Markup {
    let (language, t) = (visit.language, &visit.t);
    layout(
        language,
        t,
        path,
        title,
        &html! {},
        &html! {
            p.member-nav {
                @if visit.user.is_some() {
                    a href=(href("/lessons", language)) { (t.lessons_title) }
                    " · "
                    a href=(href("/account", language)) { (t.account_title) }
                } @else {
                    a href=(href("/login", language)) { (t.login_title) }
                    " · "
                    a href=(href("/register", language)) { (t.register_title) }
                }
            }
            (content)
        },
    )
}

/// The form asking for a link to log in or to register. `email` is what was
/// typed before, if anything.
pub fn email_form(t: &Translations, language: Language, kind: LinkKind, email: &str) -> Markup {
    let action = match kind {
        LinkKind::Login => "/login",
        LinkKind::Register => "/register",
    };
    html! {
        form.email-form method="post" action=(href(action, language)) {
            label for="email" { (t.email_label) }
            input #email type="email" name="email" value=(email) required autocomplete="email";
            button type="submit" { (t.send_link) }
        }
    }
}

/// The login or registration page, with `error` above the form.
fn email_page(visit: &Visit, kind: LinkKind, email: &str, error: Option<&str>) -> Markup {
    let (language, t) = (visit.language, &visit.t);
    let (path, title, intro) = match kind {
        LinkKind::Login => ("/login", t.login_title, t.login_intro),
        LinkKind::Register => ("/register", t.register_title, t.register_intro),
    };
    let other_href = match kind {
        LinkKind::Login => href("/register", language),
        LinkKind::Register => href("/login", language),
    };
    let other = match kind {
        LinkKind::Login => linked_markup(
            t.no_account,
            &[("register", other_href.as_str(), t.register_title)],
        ),
        LinkKind::Register => linked_markup(
            t.have_account,
            &[("login", other_href.as_str(), t.login_title)],
        ),
    };
    page(
        visit,
        path,
        title,
        &html! {
            h2 { (title) }
            p { (intro) }
            @if let Some(error) = error {
                p.error role="alert" { (error) }
            }
            (email_form(t, language, kind, email))
            p { (other) }
        },
    )
}

async fn login_page(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    show_email_page(&members, query, &headers, LinkKind::Login).await
}

async fn register_page(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    show_email_page(&members, query, &headers, LinkKind::Register).await
}

async fn show_email_page(
    members: &Members,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: &HeaderMap,
    kind: LinkKind,
) -> Page {
    let visit = members.visit(query, headers).await?;
    if visit.user.is_some() {
        return Ok(redirect("/lessons", visit.language));
    }
    Ok((page_headers(), email_page(&visit, kind, "", None)).into_response())
}

#[derive(Deserialize)]
struct EmailForm {
    email: String,
}

async fn login(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
    Form(form): Form<EmailForm>,
) -> Page {
    send_link(&members, query, &headers, LinkKind::Login, &form.email).await
}

async fn register(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
    Form(form): Form<EmailForm>,
) -> Page {
    send_link(&members, query, &headers, LinkKind::Register, &form.email).await
}

async fn send_link(
    members: &Members,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: &HeaderMap,
    kind: LinkKind,
    typed: &str,
) -> Page {
    let visit = members.visit(query, headers).await?;
    let (language, t) = (visit.language, &visit.t);
    let Some(email) = accounts::parse_email(typed) else {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            page_headers(),
            email_page(&visit, kind, typed, Some(t.invalid_email)),
        )
            .into_response());
    };
    if let Some(token) = members
        .accounts
        .new_link(&email, language, kind, Utc::now())
        .await?
    {
        let link = format!(
            "{}/login/confirm?token={token}&lang={}",
            members.site_url,
            language.code()
        );
        let sent = match mail::login_link(&members.from, &email, language, kind, &link) {
            Ok(message) => members.mailer.send(message).await,
            Err(error) => Err(error),
        };
        if matches!(members.mailer, Mailer::Log) {
            // The email isn't sent in development, so the link is shown here.
            tracing::info!(%link, "Link in the email");
        }
        if let Err(error) = sent {
            tracing::error!(%error, ?kind, "Failed to send a login link");
            members.accounts.forget_link(&token).await?;
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                page_headers(),
                email_page(&visit, kind, &email, Some(t.send_failed)),
            )
                .into_response());
        }
    }
    let (path, message) = match kind {
        LinkKind::Login => ("/login", t.link_sent_login),
        LinkKind::Register => ("/register", t.link_sent_register),
    };
    Ok((
        page_headers(),
        page(
            &visit,
            path,
            t.link_sent_title,
            &html! {
                h2 { (t.link_sent_title) }
                p { (message) }
            },
        ),
    )
        .into_response())
}

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}

/// The page a link in an email opens. Logging in takes a press of a button,
/// as some mail services open the links in emails to check them, which
/// would use the link up.
async fn confirm_page(
    State(members): State<Arc<Members>>,
    token: Result<Query<TokenQuery>, QueryRejection>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let Ok(Query(TokenQuery { token })) = token else {
        return Ok(expired_link(&visit));
    };
    let (language, t) = (visit.language, &visit.t);
    Ok((
        page_headers(),
        page(
            &visit,
            "/login",
            t.login_title,
            &html! {
                h2 { (t.login_title) }
                p { (t.confirm_intro) }
                form.email-form method="post" action=(href("/login/confirm", language)) {
                    input type="hidden" name="token" value=(token);
                    button type="submit" { (t.login_title) }
                }
            },
        ),
    )
        .into_response())
}

fn expired_link(visit: &Visit) -> Response {
    let t = &visit.t;
    let login = href("/login", visit.language);
    (
        StatusCode::GONE,
        page_headers(),
        page(
            visit,
            "/login",
            t.login_title,
            &html! {
                h2 { (t.login_title) }
                p { (linked_markup(t.link_expired, &[("login", login.as_str(), t.new_link)])) }
            },
        ),
    )
        .into_response()
}

#[derive(Deserialize)]
struct TokenForm {
    token: String,
}

async fn confirm(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let now = Utc::now();
    let Some(user) = members.accounts.use_link(&form.token, now).await? else {
        return Ok(expired_link(&visit));
    };
    if let Some(old) = &visit.session {
        members.accounts.end_session(old).await?;
    }
    let token = members.accounts.new_session(user.id, now).await?;
    tracing::info!(user = user.id, "Logged in");
    Ok((
        [(header::SET_COOKIE, members.session_cookie(&token))],
        redirect("/lessons", visit.language),
    )
        .into_response())
}

async fn logout(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    if let Some(token) = &visit.session {
        members.accounts.end_session(token).await?;
    }
    Ok((
        [(header::SET_COOKIE, members.removed_cookie())],
        redirect("/", visit.language),
    )
        .into_response())
}

async fn account_page(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let Some(user) = &visit.user else {
        return Ok(redirect("/login", visit.language));
    };
    let (language, t) = (visit.language, &visit.t);
    let member = user.is_member(Utc::now());
    let membership = match (&user.member_until, member) {
        (Some(until), true) => t.member_until.replace("{date}", &date(until, language)),
        _ => t.not_member.to_string(),
    };
    Ok((
        page_headers(),
        page(
            &visit,
            "/account",
            t.account_title,
            &html! {
                h2 { (t.account_title) }
                p { (t.logged_in_as) " " strong { (user.email) } }
                p { (membership) }
                form.email-form method="post" action=(href("/logout", language)) {
                    button type="submit" { (t.logout) }
                }
                @if members.dev {
                    form.email-form method="post" action=(href("/account/dev-membership", language)) {
                        button type="submit" {
                            @if member { "Development: end membership" } @else { "Development: start a month's membership" }
                        }
                    }
                }
                h2 { (t.delete_title) }
                p { (t.delete_info) }
                form.email-form method="post" action=(href("/account/delete", language)) {
                    label.checkbox {
                        input type="checkbox" name="confirm" value="yes" required;
                        " " (t.delete_confirm)
                    }
                    button type="submit" { (t.delete_title) }
                }
            },
        ),
    )
        .into_response())
}

/// A stored time's date as written in `language`: 2026-12-24 or 24.12.2026.
fn date(time: &str, language: Language) -> String {
    let date = time.get(..10).unwrap_or(time);
    let parts: Vec<&str> = date.splitn(3, '-').collect();
    match (language, parts.as_slice()) {
        (Language::Fi, [year, month, day]) => format!(
            "{}.{}.{year}",
            day.trim_start_matches('0'),
            month.trim_start_matches('0')
        ),
        _ => date.to_string(),
    }
}

#[derive(Deserialize)]
struct DeleteForm {
    confirm: Option<String>,
}

async fn delete_account(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> Page {
    let mut visit = members.visit(query, &headers).await?;
    let Some(user) = visit.user.take() else {
        return Ok(redirect("/login", visit.language));
    };
    if form.confirm.as_deref() != Some("yes") {
        return Ok(redirect("/account", visit.language));
    }
    members.accounts.delete_user(user.id).await?;
    tracing::info!(user = user.id, "Deleted an account");
    let t = &visit.t;
    Ok((
        [(header::SET_COOKIE, members.removed_cookie())],
        page_headers(),
        page(
            &visit,
            "/",
            t.delete_title,
            &html! {
                h2 { (t.delete_title) }
                p { (t.account_deleted) }
            },
        ),
    )
        .into_response())
}

/// Development only: starts a month's membership, or ends one, to try the
/// lessons as a member before payments are made.
async fn dev_membership(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let Some(user) = &visit.user else {
        return Ok(redirect("/login", visit.language));
    };
    let now = Utc::now();
    let until = (!user.is_member(now)).then(|| now + Duration::days(30));
    members.accounts.set_member_until(user.id, until).await?;
    Ok(redirect("/account", visit.language))
}

/// The address of a set's page, or of a lesson's page with `lesson`.
fn set_href(set: &Set, lesson: Option<&Lesson>, language: Language) -> String {
    match lesson {
        Some(lesson) => href(&format!("/lessons/{}/{}", set.slug, lesson.slug), language),
        None => href(&format!("/lessons/{}", set.slug), language),
    }
}

/// E.g. "Movement 1" or "Movements 5, 30".
fn movements_text(t: &Translations, movements: &[u8]) -> String {
    let label = if movements.len() == 1 {
        t.movement
    } else {
        t.movements
    };
    let numbers: Vec<String> = movements.iter().map(u8::to_string).collect();
    format!("{label} {}", numbers.join(", "))
}

fn passing_text(t: &Translations, passing: Passing) -> &'static str {
    match passing {
        Passing::Practice => t.passing_practice,
        Passing::Quiz => t.passing_quiz,
        Passing::Review => t.passing_review,
    }
}

/// A logged in user's membership and passed lessons.
struct Progress {
    member: bool,
    completed: HashSet<String>,
}

impl Progress {
    fn learner(&self) -> Learner<'_> {
        Learner {
            member: self.member,
            completed: &self.completed,
        }
    }
}

impl Members {
    /// The progress of `visit`'s user, if logged in.
    async fn progress(&self, visit: &Visit) -> Result<Option<Progress>, Failure> {
        let Some(user) = &visit.user else {
            return Ok(None);
        };
        Ok(Some(Progress {
            member: user.is_member(Utc::now()),
            completed: self.accounts.completed(user.id).await?,
        }))
    }

    fn access(&self, id: LessonId, progress: Option<&Progress>) -> Access {
        self.course
            .access(id, progress.map(Progress::learner).as_ref())
    }

    /// Whether a lesson of a form can be passed by marking it practised.
    fn can_mark_done(&self, lesson: &Lesson) -> bool {
        match lesson.passing {
            Some(Passing::Practice) => true,
            Some(_) => self.dev,
            None => false,
        }
    }

    /// Why a locked lesson or set isn't open yet, with links to what opens it.
    fn lock_text(&self, t: &Translations, language: Language, access: Access) -> Markup {
        match access {
            Access::NeedsMembership => html! { (t.needs_membership) },
            Access::NeedsSet(set) => {
                let Some(set) = self.course.sets().get(set) else {
                    return html! {};
                };
                let link = set_href(set, None, language);
                linked_markup(t.needs_set, &[("set", link.as_str(), set.title(language))])
            }
            Access::NeedsLesson(id) => {
                let (Some(set), Some(lesson)) =
                    (self.course.sets().get(id.set), self.course.lesson(id))
                else {
                    return html! {};
                };
                let link = set_href(set, Some(lesson), language);
                linked_markup(
                    t.needs_lesson,
                    &[("lesson", link.as_str(), lesson.title(language))],
                )
            }
            Access::NeedsPrevious => html! { (t.needs_previous) },
            Access::Open | Access::Done | Access::NeedsLogin => html! {},
        }
    }
}

fn needs_login(t: &Translations, language: Language) -> Markup {
    let login = href("/login", language);
    let register = href("/register", language);
    html! {
        p {
            (linked_markup(t.needs_login, &[
                ("login", login.as_str(), t.login_inline),
                ("register", register.as_str(), t.register_inline),
            ]))
        }
    }
}

/// The sets with how far the visitor has got in each.
async fn course_page(
    State(members): State<Arc<Members>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let progress = members.progress(&visit).await?;
    let (language, t) = (visit.language, &visit.t);
    let status = |position: usize, set: &Set| -> Markup {
        let total = set.lessons.len();
        let count = match set.kind {
            Kind::Form => t.lesson_count,
            Kind::Practices => t.practice_count,
        };
        let Some(progress) = &progress else {
            return html! { (count.replace("{count}", &total.to_string())) };
        };
        if let Some(required) = members.course.missing_set(position, &progress.completed) {
            return members.lock_text(t, language, Access::NeedsSet(required));
        }
        let passed = members.course.passed(position, &progress.completed);
        match set.kind {
            Kind::Practices => html! { (count.replace("{count}", &total.to_string())) },
            Kind::Form if passed == total => html! { (t.set_finished) },
            Kind::Form => html! {
                (t.set_progress
                    .replace("{passed}", &passed.to_string())
                    .replace("{total}", &total.to_string()))
            },
        }
    };
    Ok((
        page_headers(),
        page(
            &visit,
            "/lessons",
            t.lessons_title,
            &html! {
                h2 { (t.lessons_title) }
                @if visit.user.is_none() {
                    (needs_login(t, language))
                }
                ul.lessons {
                    @for (position, set) in members.course.sets().iter().enumerate() {
                        li {
                            a href=(set_href(set, None, language)) { (set.title(language)) }
                            span.lesson-meta { " · " (status(position, set)) }
                        }
                    }
                }
            },
        ),
    )
        .into_response())
}

/// A set's lessons, and which of them the visitor may open.
async fn set_page(
    State(members): State<Arc<Members>>,
    UrlPath(set_slug): UrlPath<String>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let language = visit.language;
    let Some((position, set)) = members.course.find_set(&set_slug) else {
        return Ok(not_found(language));
    };
    let progress = members.progress(&visit).await?;
    let t = &visit.t;
    let missing = progress
        .as_ref()
        .and_then(|progress| members.course.missing_set(position, &progress.completed));
    let item = |lesson_position: usize, lesson: &Lesson| {
        let id = LessonId {
            set: position,
            lesson: lesson_position,
        };
        let access = members.access(id, progress.as_ref());
        html! {
            li {
                @if access.is_open() {
                    a href=(set_href(set, Some(lesson), language)) { (lesson.title(language)) }
                } @else {
                    (lesson.title(language))
                }
                span.lesson-meta {
                    @if set.kind == Kind::Form {
                        " · " (movements_text(t, &lesson.movements))
                    }
                    @if lesson.free { " · " (t.free) }
                    @match access {
                        Access::Done => { " · " (t.done) }
                        Access::Open | Access::NeedsLogin => {}
                        _ => { " · " (t.locked) }
                    }
                }
            }
        }
    };
    Ok((
        page_headers(),
        page(
            &visit,
            &format!("/lessons/{}", set.slug),
            set.title(language),
            &html! {
                h2 { (set.title(language)) }
                @if visit.user.is_none() {
                    (needs_login(t, language))
                }
                @if let Some(required) = missing {
                    p { (members.lock_text(t, language, Access::NeedsSet(required))) }
                }
                @if set.kind == Kind::Form {
                    ol.lessons {
                        @for (lesson_position, lesson) in set.lessons.iter().enumerate() {
                            (item(lesson_position, lesson))
                        }
                    }
                } @else {
                    ul.lessons {
                        @for (lesson_position, lesson) in set.lessons.iter().enumerate() {
                            (item(lesson_position, lesson))
                        }
                    }
                }
                p.lesson-nav {
                    a href=(href("/lessons", language)) { (t.all_lessons) }
                }
            },
        ),
    )
        .into_response())
}

async fn lesson_page(
    State(members): State<Arc<Members>>,
    UrlPath((set_slug, lesson_slug)): UrlPath<(String, String)>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let language = visit.language;
    let Some((id, lesson)) = members.course.find(&set_slug, &lesson_slug) else {
        return Ok(not_found(language));
    };
    let set = &members.course.sets()[id.set];
    let progress = members.progress(&visit).await?;
    let access = members.access(id, progress.as_ref());
    if access == Access::NeedsLogin {
        return Ok(redirect("/login", language));
    }
    let t = &visit.t;
    let previous = id
        .lesson
        .checked_sub(1)
        .and_then(|previous| set.lessons.get(previous));
    let next = set.lessons.get(id.lesson + 1);
    let number = t
        .lesson_number
        .replace("{number}", &(id.lesson + 1).to_string());
    let content = html! {
        p.lesson-meta {
            a href=(set_href(set, None, language)) { (set.title(language)) }
            @if set.kind == Kind::Form {
                " · " (number) " · " (movements_text(t, &lesson.movements))
            }
            @if lesson.free { " · " (t.free) }
        }
        h2 { (lesson.title(language)) }
        @if access.is_open() {
            @if lesson.video(language).is_some() {
                video.lesson-video controls preload="metadata" controlslist="nodownload"
                    src=(href(&format!("/lessons/{}/{}/video", set.slug, lesson.slug), language)) {
                    (t.video_unsupported)
                }
                p.video-notice { (t.video_notice) }
            }
            div.lesson-text { (PreEscaped(lesson.html(language))) }
            @if let Some(passing) = lesson.passing {
                p.passing { (t.passing_label) ": " (passing_text(t, passing)) }
                @if access == Access::Done {
                    p { (t.lesson_done) }
                } @else if members.can_mark_done(lesson) {
                    @if passing != Passing::Practice {
                        p.dev-note { (DEV_PASSING_NOTE) }
                    }
                    form.email-form method="post" action=(href(&format!("/lessons/{}/{}/complete", set.slug, lesson.slug), language)) {
                        button type="submit" { (t.mark_done) }
                    }
                }
            }
        } @else {
            p { (members.lock_text(t, language, access)) }
        }
        p.lesson-nav {
            @if let Some(previous) = previous {
                a href=(set_href(set, Some(previous), language)) rel="prev" { "← " (t.previous_lesson) }
                " · "
            }
            a href=(set_href(set, None, language)) { (set.title(language)) }
            @if let Some(next) = next {
                " · "
                a href=(set_href(set, Some(next), language)) rel="next" { (t.next_lesson) " →" }
            }
        }
    };
    let status = if access.is_open() {
        StatusCode::OK
    } else {
        StatusCode::FORBIDDEN
    };
    Ok((
        status,
        page_headers(),
        page(
            &visit,
            &format!("/lessons/{}/{}", set.slug, lesson.slug),
            lesson.title(language),
            &content,
        ),
    )
        .into_response())
}

/// A lesson's video, for those who may open the lesson. The browser asks
/// for it in parts (Range requests) as it plays and seeks.
async fn lesson_video(
    State(members): State<Arc<Members>>,
    UrlPath((set_slug, lesson_slug)): UrlPath<(String, String)>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    request: Request,
) -> Page {
    let visit = members.visit(query, request.headers()).await?;
    let language = visit.language;
    let Some((id, lesson)) = members.course.find(&set_slug, &lesson_slug) else {
        return Ok(not_found(language));
    };
    let progress = members.progress(&visit).await?;
    match members.access(id, progress.as_ref()) {
        access if access.is_open() => {}
        Access::NeedsLogin => return Ok(redirect("/login", language)),
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }
    let Some(path) = lesson.video(language) else {
        return Ok(not_found(language));
    };
    let Ok(mut response) = ServeFile::new(path).oneshot(request).await;
    let response_headers = response.headers_mut();
    for (name, value) in page_headers() {
        response_headers.insert(name, value);
    }
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    Ok(response.into_response())
}

async fn complete_lesson(
    State(members): State<Arc<Members>>,
    UrlPath((set_slug, lesson_slug)): UrlPath<(String, String)>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Page {
    let visit = members.visit(query, &headers).await?;
    let language = visit.language;
    let Some((id, lesson)) = members.course.find(&set_slug, &lesson_slug) else {
        return Ok(not_found(language));
    };
    let Some(user) = &visit.user else {
        return Ok(redirect("/login", language));
    };
    let set = &members.course.sets()[id.set];
    let progress = members.progress(&visit).await?;
    if members.access(id, progress.as_ref()) != Access::Open || !members.can_mark_done(lesson) {
        // Passed already, locked, or passed some other way: back to the lesson.
        return Ok(Redirect::to(&set_href(set, Some(lesson), language)).into_response());
    }
    members
        .accounts
        .complete(user.id, &lesson.key, Utc::now())
        .await?;
    // On to the next lesson, or after the last one, to the sets, where the
    // next set may have opened.
    Ok(match set.lessons.get(id.lesson + 1) {
        Some(next) => Redirect::to(&set_href(set, Some(next), language)).into_response(),
        None => redirect("/lessons", language),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use lettre::transport::stub::AsyncStubTransport;
    use tower::ServiceExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Site {
        app: Router,
        members: Arc<Members>,
        outbox: AsyncStubTransport,
    }

    async fn members(
        dev: bool,
        outbox: &AsyncStubTransport,
    ) -> Result<Arc<Members>, Box<dyn std::error::Error>> {
        Ok(Arc::new(Members {
            course: Course::load(Path::new("tests/lessons"))?,
            accounts: Accounts::new(crate::db::in_memory().await?),
            mailer: Mailer::Stub(outbox.clone()),
            from: mail::DEFAULT_FROM.parse()?,
            site_url: "https://example.test".to_string(),
            dev,
        }))
    }

    async fn site(dev: bool) -> Result<Site, Box<dyn std::error::Error>> {
        site_with(dev, AsyncStubTransport::new_ok()).await
    }

    async fn site_with(
        dev: bool,
        outbox: AsyncStubTransport,
    ) -> Result<Site, Box<dyn std::error::Error>> {
        let members = members(dev, &outbox).await?;
        let app = crate::app(
            Path::new("dist"),
            dev,
            "https://example.test",
            Some(members.clone()),
        );
        Ok(Site {
            app,
            members,
            outbox,
        })
    }

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    impl Reply {
        fn location(&self) -> &str {
            self.headers
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        }

        /// The session secret set by the reply, if any.
        fn session(&self) -> Option<String> {
            let cookie = self.headers.get(header::SET_COOKIE)?.to_str().ok()?;
            let value = cookie.strip_prefix("session=")?.split(';').next()?;
            (!value.is_empty()).then(|| value.to_string())
        }
    }

    impl Site {
        async fn request(
            &self,
            method: &str,
            uri: &str,
            session: Option<&str>,
            form: Option<&str>,
        ) -> Result<Reply, Box<dyn std::error::Error>> {
            let mut request = Request::builder().method(method).uri(uri);
            if let Some(session) = session {
                request = request.header(header::COOKIE, format!("theme=dark; session={session}"));
            }
            if form.is_some() {
                request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            }
            let body = form.map_or_else(Body::empty, |form| Body::from(form.to_string()));
            let response = self.app.clone().oneshot(request.body(body)?).await?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
            Ok(Reply {
                status,
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            })
        }

        async fn get(
            &self,
            uri: &str,
            session: Option<&str>,
        ) -> Result<Reply, Box<dyn std::error::Error>> {
            self.request("GET", uri, session, None).await
        }

        async fn post(
            &self,
            uri: &str,
            session: Option<&str>,
            form: &str,
        ) -> Result<Reply, Box<dyn std::error::Error>> {
            self.request("POST", uri, session, Some(form)).await
        }

        /// The secrets in the links of the emails sent so far.
        async fn sent_tokens(&self) -> Vec<String> {
            self.outbox
                .messages()
                .await
                .iter()
                .filter_map(|(_, raw)| {
                    // Undo quoted-printable line breaks and escapes.
                    let raw = raw.replace("=\r\n", "").replace("=3D", "=");
                    let start = raw.find("token=")? + "token=".len();
                    Some(raw[start..start + 64].to_string())
                })
                .collect()
        }

        /// Registers `email` and logs in, returning the session secret.
        async fn register(&self, email: &str) -> Result<String, Box<dyn std::error::Error>> {
            let before = self.sent_tokens().await.len();
            let reply = self
                .post("/register?lang=en", None, &format!("email={email}"))
                .await?;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
            let tokens = self.sent_tokens().await;
            assert_eq!(tokens.len(), before + 1);
            let token = tokens.last().ok_or("no email")?;
            let reply = self
                .post("/login/confirm?lang=en", None, &format!("token={token}"))
                .await?;
            assert_eq!(reply.status, StatusCode::SEE_OTHER);
            assert_eq!(reply.location(), "/lessons?lang=en");
            Ok(reply.session().ok_or("no session cookie")?)
        }
    }

    #[test]
    fn dates_are_written_as_usual_in_each_language() {
        assert_eq!(date("2026-12-04T10:00:00Z", Language::Fi), "4.12.2026");
        assert_eq!(date("2026-12-04T10:00:00Z", Language::En), "2026-12-04");
    }

    #[tokio::test]
    async fn registering_sends_a_link_that_logs_in() -> TestResult {
        let site = site(false).await?;
        let reply = site.get("/register?lang=fi", None).await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.headers[header::CACHE_CONTROL], "private, no-store");
        assert!(
            reply
                .body
                .contains(r#"<form class="email-form" method="post" action="/register?lang=fi">"#)
        );

        let reply = site
            .post("/register?lang=fi", None, "email=ann%40example.fi")
            .await?;
        assert!(
            reply
                .body
                .contains("Linkki rekisteröitymisen viimeistelyyn on matkalla")
        );
        let messages = site.outbox.messages().await;
        let (envelope, raw) = messages.first().ok_or("no email")?;
        assert_eq!(
            envelope
                .to()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["ann@example.fi"]
        );
        assert!(
            raw.contains("https://example.test/login/confirm?token="),
            "{raw}"
        );
        let token = site.sent_tokens().await.pop().ok_or("no token")?;

        // Opening the link only shows a button, so a mail service checking
        // the link doesn't use it up.
        let reply = site
            .get(&format!("/login/confirm?token={token}&lang=fi"), None)
            .await?;
        assert!(reply.body.contains(&format!(
            r#"<input type="hidden" name="token" value="{token}">"#
        )));
        let reply = site
            .post("/login/confirm?lang=fi", None, &format!("token={token}"))
            .await?;
        assert_eq!(reply.status, StatusCode::SEE_OTHER);
        assert_eq!(reply.location(), "/lessons?lang=fi");
        let cookie = reply.headers[header::SET_COOKIE].to_str()?;
        assert!(
            cookie.ends_with("; Path=/; Max-Age=2592000; HttpOnly; SameSite=Lax; Secure"),
            "{cookie}"
        );
        let session = reply.session().ok_or("no session")?;

        let reply = site.get("/account?lang=en", Some(&session)).await?;
        assert!(
            reply
                .body
                .contains("Logged in as <strong>ann@example.fi</strong>")
        );
        assert!(!reply.body.contains("Development"));

        // The link works once.
        let reply = site
            .post("/login/confirm?lang=en", None, &format!("token={token}"))
            .await?;
        assert_eq!(reply.status, StatusCode::GONE);
        assert!(
            reply
                .body
                .contains(r#"<a href="/login?lang=en">Ask for a new link</a>"#)
        );
        Ok(())
    }

    #[tokio::test]
    async fn login_tells_nothing_about_who_has_an_account() -> TestResult {
        let site = site(false).await?;
        let reply = site
            .post("/login?lang=en", None, "email=nobody%40example.fi")
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(
            reply
                .body
                .contains("If there is an account for this address")
        );
        assert_eq!(site.sent_tokens().await, [] as [std::string::String; 0]);

        site.register("ann@example.fi").await?;
        let reply = site
            .post("/login?lang=en", None, "email=ann%40example.fi")
            .await?;
        assert!(
            reply
                .body
                .contains("If there is an account for this address")
        );
        assert_eq!(site.sent_tokens().await.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_addresses_are_asked_again() -> TestResult {
        let site = site(false).await?;
        let reply = site.post("/register?lang=en", None, "email=ann%40").await?;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(reply.body.contains("Please check the email address."));
        assert!(reply.body.contains(r#"value="ann@""#));
        assert_eq!(site.sent_tokens().await, [] as [std::string::String; 0]);
        Ok(())
    }

    #[tokio::test]
    async fn failed_emails_can_be_asked_again_at_once() -> TestResult {
        let site = site_with(false, AsyncStubTransport::new_error()).await?;
        // Twice: the failed link doesn't count as sent moments ago.
        for _ in 0..2 {
            let reply = site
                .post("/register?lang=en", None, "email=ann%40example.fi")
                .await?;
            assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(reply.body.contains("The email could not be sent."));
        }
        Ok(())
    }

    #[tokio::test]
    async fn visitors_see_the_lessons_but_must_log_in_to_open_them() -> TestResult {
        let site = site(false).await?;
        let reply = site.get("/lessons?lang=en", None).await?;
        assert!(reply.body.contains(r#"<li><a href="/lessons/slow-set?lang=en">Slow Set</a><span class="lesson-meta"> · 3 lessons</span></li>"#));
        assert_eq!(reply.status, StatusCode::OK);
        assert!(reply.body.contains(r#"<a href="/login?lang=en">Log in</a> or <a href="/register?lang=en">register</a> to start the lessons."#));
        let reply = site.get("/lessons/slow-set?lang=en", None).await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(reply.body.contains(
            "<li>Preparation<span class=\"lesson-meta\"> · Movement 1 · Free</span></li>"
        ));
        assert!(
            !reply
                .body
                .contains(r#"href="/lessons/slow-set/preparation"#)
        );

        for uri in ["/lessons/slow-set/preparation?lang=en", "/account?lang=en"] {
            let reply = site.get(uri, None).await?;
            assert_eq!(reply.status, StatusCode::SEE_OTHER, "{uri}");
            assert_eq!(reply.location(), "/login?lang=en", "{uri}");
        }
        let reply = site
            .post("/lessons/slow-set/preparation/complete?lang=en", None, "")
            .await?;
        assert_eq!(reply.location(), "/login?lang=en");
        let reply = site.get("/lessons/missing?lang=fi", None).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn free_lessons_are_open_and_paid_ones_wait_for_membership() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        let reply = site
            .get("/lessons/slow-set?lang=en", Some(&session))
            .await?;
        assert!(
            reply
                .body
                .contains(r#"<a href="/lessons/slow-set/preparation?lang=en">Preparation</a>"#)
        );
        assert!(
            reply
                .body
                .contains(r#"<a href="/lessons/slow-set/opening?lang=en">Opening</a>"#)
        );
        assert!(reply.body.contains(
            "<li>Single Whip<span class=\"lesson-meta\"> · Movements 5, 30 · Locked</span></li>"
        ));

        let reply = site
            .get("/lessons/slow-set/opening?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(reply.body.contains(
            r#"<p class="lesson-meta"><a href="/lessons/slow-set?lang=en">Slow Set</a> · Lesson 2 · Movements 2, 3 · Free</p><h2>Opening</h2><video "#
        ));
        assert!(
            reply
                .body
                .contains(r#"<div class="lesson-text"><p>Raise the arms.</p>"#)
        );
        assert!(reply.body.contains("To pass: answer a quiz"));
        // Quizzes aren't made yet, so outside development there is no way to pass it.
        assert!(!reply.body.contains("Mark as practised"));
        assert!(
            reply
                .body
                .contains(r#"<a href="/lessons/slow-set/single-whip?lang=en" rel="next">"#)
        );
        assert!(
            reply
                .body
                .contains(r#"<a href="/lessons/slow-set/opening?lang=fi" lang="fi">"#)
        );

        let reply = site
            .get("/lessons/slow-set/single-whip?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(reply.body.contains("This lesson is for members."));
        assert!(!reply.body.contains("Hook the hand"));
        Ok(())
    }

    #[tokio::test]
    async fn members_pass_lessons_in_order() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        let user = site
            .members
            .accounts
            .user_by_email("ann@example.fi")
            .await?
            .ok_or("no user")?;
        site.members
            .accounts
            .set_member_until(user.id, Some(Utc::now() + Duration::days(30)))
            .await?;

        let reply = site
            .get("/lessons/slow-set/single-whip?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(
            reply
                .body
                .contains("opens when you have passed the one before it")
        );

        let reply = site
            .get("/lessons/slow-set/preparation?lang=fi", Some(&session))
            .await?;
        assert!(reply.body.contains("<h3>Jalat</h3>"));
        assert!(reply.body.contains(r#"<form class="email-form" method="post" action="/lessons/slow-set/preparation/complete?lang=fi"><button type="submit">Merkitse harjoitelluksi</button></form>"#));
        let reply = site
            .post(
                "/lessons/slow-set/preparation/complete?lang=fi",
                Some(&session),
                "",
            )
            .await?;
        assert_eq!(reply.location(), "/lessons/slow-set/opening?lang=fi");
        let reply = site
            .get("/lessons/slow-set/preparation?lang=en", Some(&session))
            .await?;
        assert!(reply.body.contains("You have passed this lesson."));

        // A quiz lesson can't be marked practised outside development.
        let reply = site
            .post(
                "/lessons/slow-set/opening/complete?lang=en",
                Some(&session),
                "",
            )
            .await?;
        assert_eq!(reply.location(), "/lessons/slow-set/opening?lang=en");
        assert_eq!(site.members.accounts.completed(user.id).await?.len(), 1);

        site.members
            .accounts
            .complete(user.id, "slow-set/opening", Utc::now())
            .await?;
        let reply = site
            .get("/lessons/slow-set/single-whip?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(reply.body.contains("Hook the hand"));
        let reply = site
            .get("/lessons/slow-set?lang=en", Some(&session))
            .await?;
        assert!(reply.body.contains("Movement 1 · Free · Passed</span>"));
        Ok(())
    }

    #[tokio::test]
    async fn finishing_a_set_opens_the_next_and_practices_open_on_their_own() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        let user = site
            .members
            .accounts
            .user_by_email("ann@example.fi")
            .await?
            .ok_or("no user")?;

        // Without membership only the free practice is open.
        let reply = site
            .get("/lessons/practices?lang=en", Some(&session))
            .await?;
        assert!(reply.body.contains(r#"<ul class="lessons"><li><a href="/lessons/practices/standing?lang=en">Standing</a><span class="lesson-meta"> · Free</span></li><li>Silk Reeling<span class="lesson-meta"> · Locked</span></li>"#));
        let reply = site
            .get("/lessons/practices/standing?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert!(!reply.body.contains("To pass"));
        assert!(!reply.body.contains("Mark as practised"));

        site.members
            .accounts
            .set_member_until(user.id, Some(Utc::now() + Duration::days(30)))
            .await?;
        let reply = site
            .get("/lessons/practices/silk-reeling?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        let reply = site
            .get("/lessons/practices/push-hands?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(reply.body.contains(
            r#"Opens when you have passed <a href="/lessons/slow-set/opening?lang=en">Opening</a>."#
        ));

        let reply = site.get("/lessons?lang=en", Some(&session)).await?;
        assert!(
            reply.body.contains(
                "Slow Set</a><span class=\"lesson-meta\"> · 0 of 3 lessons passed</span>"
            )
        );
        assert!(reply.body.contains(r#"Fast Set</a><span class="lesson-meta"> · Opens when <a href="/lessons/slow-set?lang=en">Slow Set</a> is finished.</span>"#));
        let reply = site
            .get("/lessons/fast-set?lang=fi", Some(&session))
            .await?;
        assert!(reply.body.contains(
            r#"Avautuu, kun <a href="/lessons/slow-set?lang=fi">Hidas sarja</a> on suoritettu."#
        ));
        assert!(
            reply
                .body
                .contains("Nopea avaus<span class=\"lesson-meta\"> · Liike 1 · Lukittu</span>")
        );
        let reply = site
            .get("/lessons/fast-set/fast-opening?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(!reply.body.contains("Practise fast-opening"));

        for key in ["slow-set/preparation", "slow-set/opening"] {
            site.members
                .accounts
                .complete(user.id, key, Utc::now())
                .await?;
        }
        let reply = site
            .get("/lessons/practices/push-hands?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        // single-whip is passed by a review, so it is marked here directly.
        site.members
            .accounts
            .complete(user.id, "slow-set/single-whip", Utc::now())
            .await?;
        let reply = site.get("/lessons?lang=en", Some(&session)).await?;
        assert!(
            reply
                .body
                .contains("Slow Set</a><span class=\"lesson-meta\"> · Finished</span>")
        );
        assert!(
            reply.body.contains(
                "Fast Set</a><span class=\"lesson-meta\"> · 0 of 1 lessons passed</span>"
            )
        );

        // Passing the last lesson of a set leads back to the sets.
        let reply = site
            .post(
                "/lessons/fast-set/fast-opening/complete?lang=en",
                Some(&session),
                "",
            )
            .await?;
        assert_eq!(reply.location(), "/lessons?lang=en");
        Ok(())
    }

    #[tokio::test]
    async fn development_lets_any_lesson_be_marked_and_membership_toggled() -> TestResult {
        let site = site(true).await?;
        let session = site.register("ann@example.fi").await?;
        let reply = site
            .get("/lessons/slow-set/opening?lang=en", Some(&session))
            .await?;
        assert!(reply.body.contains("Development only: quizzes"));
        assert!(reply.body.contains("Mark as practised"));

        let reply = site.get("/account?lang=en", Some(&session)).await?;
        assert!(reply.body.contains("Development: start a month"));
        assert!(reply.body.contains("No membership yet"));
        site.post("/account/dev-membership?lang=en", Some(&session), "")
            .await?;
        let reply = site.get("/account?lang=en", Some(&session)).await?;
        assert!(
            reply.body.contains("Membership paid until 20"),
            "{}",
            reply.body
        );
        site.post("/account/dev-membership?lang=en", Some(&session), "")
            .await?;
        let reply = site.get("/account?lang=en", Some(&session)).await?;
        assert!(reply.body.contains("No membership yet"));

        let production = self::site(false).await?;
        let reply = production.post("/account/dev-membership", None, "").await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn videos_are_shown_to_those_who_may_open_the_lesson() -> TestResult {
        let site = site(false).await?;
        let reply = site
            .get("/lessons/slow-set/opening/video?lang=en", None)
            .await?;
        assert_eq!(reply.location(), "/login?lang=en");

        let session = site.register("ann@example.fi").await?;
        let reply = site
            .get("/lessons/slow-set/opening?lang=fi", Some(&session))
            .await?;
        assert!(reply.body.contains(r#"<video class="lesson-video" controls preload="metadata" controlslist="nodownload" src="/lessons/slow-set/opening/video?lang=fi">"#));
        assert!(reply.body.contains("käyttöehtojen vastaista"));
        let reply = site
            .get("/lessons/slow-set/preparation?lang=en", Some(&session))
            .await?;
        assert!(!reply.body.contains("<video"));

        let reply = site
            .get("/lessons/slow-set/opening/video?lang=en", Some(&session))
            .await?;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body, "not a real video: opening\n");
        assert_eq!(reply.headers[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(reply.headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(reply.headers[header::CONTENT_DISPOSITION], "inline");

        let request = Request::builder()
            .uri("/lessons/slow-set/opening/video?lang=en")
            .header(header::COOKIE, format!("session={session}"))
            .header(header::RANGE, "bytes=0-2")
            .header(header::ACCEPT_ENCODING, "gzip, br")
            .body(Body::empty())?;
        let response = site.app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        assert_eq!(&body[..], b"not");

        let reply = site
            .get(
                "/lessons/slow-set/single-whip/video?lang=en",
                Some(&session),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(reply.body, "");
        let reply = site
            .get(
                "/lessons/slow-set/preparation/video?lang=en",
                Some(&session),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn logging_out_ends_the_session() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        let reply = site.post("/logout?lang=fi", Some(&session), "").await?;
        assert_eq!(reply.location(), "/?lang=fi");
        assert!(
            reply.headers[header::SET_COOKIE]
                .to_str()?
                .starts_with("session=; Path=/; Max-Age=0")
        );
        let reply = site.get("/account?lang=en", Some(&session)).await?;
        assert_eq!(reply.location(), "/login?lang=en");
        Ok(())
    }

    #[tokio::test]
    async fn deleting_the_account_needs_the_box_ticked() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        let reply = site
            .post("/account/delete?lang=en", Some(&session), "")
            .await?;
        assert_eq!(reply.location(), "/account?lang=en");
        assert!(
            site.members
                .accounts
                .user_by_email("ann@example.fi")
                .await?
                .is_some()
        );

        let reply = site
            .post("/account/delete?lang=en", Some(&session), "confirm=yes")
            .await?;
        assert!(reply.body.contains("Your account has been deleted."));
        assert!(
            reply.headers[header::SET_COOKIE]
                .to_str()?
                .contains("Max-Age=0")
        );
        assert!(
            site.members
                .accounts
                .user_by_email("ann@example.fi")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn members_pages_need_nothing_the_policy_blocks() -> TestResult {
        let site = site(false).await?;
        let session = site.register("ann@example.fi").await?;
        for uri in [
            "/login",
            "/register",
            "/lessons",
            "/lessons/slow-set",
            "/lessons/slow-set/preparation",
            "/account",
        ] {
            let reply = site.get(uri, Some(&session)).await?;
            let policy = reply.headers[header::CONTENT_SECURITY_POLICY].to_str()?;
            assert!(policy.contains("form-action 'self'"), "{policy}");
            for blocked in ["<script", "<style", " style=", "<iframe", "<base"] {
                assert!(!reply.body.contains(blocked), "{uri}: {blocked}");
            }
            for action in reply.body.split(r#" action=""#).skip(1) {
                assert!(action.starts_with('/'), "{uri}: {action}");
            }
        }
        Ok(())
    }
}
