mod accounts;
mod assets;
mod db;
mod lessons;
mod mail;
mod members;
mod stripe;

use axum::{
    Router,
    extract::{Query, Request, State, rejection::QueryRejection},
    handler::HandlerWithoutStateExt,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use members::Members;
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use tower_http::{
    compression::{
        CompressionLayer, DefaultPredicate,
        predicate::{NotForContentType, Predicate},
    },
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace,
    trace::TraceLayer,
};
use tower_livereload::LiveReloadLayer;
use tracing::Level;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env before tracing so that RUST_LOG from it applies; variables
    // already set in the environment take precedence over the file.
    let dotenv = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("taichichuan_mobi=debug,tower_http=info"))
                .unwrap_or_default(),
        )
        .init();
    match dotenv {
        Ok(path) => tracing::info!(path = %path.display(), "Loaded environment file"),
        Err(error) if error.not_found() => {}
        Err(error) => {
            tracing::error!(%error, "Failed to load environment file");
            return Err(error.into());
        }
    }

    let static_dir = std::env::var("STATIC_DIR").unwrap_or("dist".to_string());
    let dev = env_flag("DEV");
    let host = std::env::var("HOST").unwrap_or("0.0.0.0".to_string());
    let port = std::env::var("PORT").unwrap_or("3000".to_string());
    // In development, links in emails lead to this computer.
    let site_url = std::env::var("SITE_URL").unwrap_or(if dev {
        format!("http://localhost:{port}")
    } else {
        DEFAULT_SITE_URL.to_string()
    });
    tracing::info!(%static_dir, %site_url, "Site settings");
    // A wrong STATIC_DIR would otherwise serve pages without styles or photos.
    let missing = assets::missing(Path::new(&static_dir));
    if !missing.is_empty() {
        tracing::error!(%static_dir, ?missing, "Static files not found, check STATIC_DIR");
        return Err(format!(
            "static files not found in {static_dir}: {}",
            missing.join(", ")
        )
        .into());
    }
    let members = match std::env::var("CONTENT_DIR") {
        Ok(content_dir) => match Members::from_env(Path::new(&content_dir), &site_url, dev).await {
            Ok(members) => Some(Arc::new(members)),
            Err(error) => {
                tracing::error!(%content_dir, %error, "Failed to set up the lessons");
                return Err(error.into());
            }
        },
        Err(_) => None,
    };
    let app = app(Path::new(&static_dir), dev, &site_url, members);
    // Behind a reverse proxy, set HOST=127.0.0.1 to accept local connections only.
    let bind_addr = if host.contains(':') {
        format!("[{host}]:{port}") // IPv6
    } else {
        format!("{host}:{port}")
    };
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|error| {
            tracing::error!(address = %bind_addr, %error, "Failed to bind HTTP listener");
            error
        })?;
    let local_addr = listener.local_addr().map_err(|error| {
        tracing::error!(address = %bind_addr, %error, "Could not determine listener address");
        error
    })?;
    tracing::info!(address = %local_addr, "HTTP server listening");
    let serve = axum::serve(listener, app.into_make_service());
    let result = if dev {
        // Live reload keeps an event stream open for each browser tab, which a
        // graceful shutdown would wait for, so dev restarts stop at once.
        tokio::select! {
            result = serve.into_future() => result,
            () = shutdown_signal() => Ok(()),
        }
    } else {
        // Finish requests in progress, e.g. when systemd stops the service.
        serve.with_graceful_shutdown(shutdown_signal()).await
    };
    if let Err(error) = result {
        tracing::error!(address = %local_addr, %error, "HTTP server failed");
        return Err(error.into());
    }
    tracing::info!(address = %local_addr, "HTTP server stopped");
    Ok(())
}

/// Completes on Ctrl-C or, on Unix, SIGTERM (sent e.g. by systemd and watchexec).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "Failed to listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "Failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("Shutdown signal received");
}

/// Set (even empty, like `DEV=` in the Justfile) and not "0" or "false".
fn env_flag(name: &str) -> bool {
    flag_enabled(std::env::var(name).ok().as_deref())
}

fn flag_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| !matches!(value, "0" | "false"))
}

/// Public address of the site, used for absolute links in the page head.
const DEFAULT_SITE_URL: &str = "https://www.taichichuan.mobi";

/// Tells browsers to use only HTTPS for this host for a year. Without
/// `includeSubDomains`, which from www would only cover *.www anyway: the bare
/// domain's redirect needs its own header from the reverse proxy. Browsers
/// ignore it over plain HTTP.
const STRICT_TRANSPORT_SECURITY: &str = "max-age=31536000";

/// The page loads only its own styles, fonts and images, and runs no scripts.
/// The logo is inline SVG markup, which needs no source of its own. Framing
/// by other sites, forms to other sites and `<base>` are refused.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'self'; font-src 'self'; \
    img-src 'self'; media-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

fn app(static_dir: &Path, dev: bool, site_url: &str, members: Option<Arc<Members>>) -> Router {
    assets::init(static_dir);
    let static_files = Router::new()
        .nest_service(
            "/static",
            // Missing files get the same not found page as missing pages.
            ServeDir::new(static_dir).not_found_service(handler_404.into_service()),
        )
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_dir.join("favicon.ico")),
        )
        .layer(middleware::from_fn_with_state(dev, cache_static));

    let site_url: Arc<str> = site_url.trim_end_matches('/').into();
    let app = Router::new()
        .route("/", get(index))
        .fallback(handler_404)
        .with_state(site_url)
        .merge(static_files);
    let app = match members {
        Some(members) => app.merge(members::router(members)),
        None => app,
    };
    let app = app.layer(
        TraceLayer::new_for_http()
            // The path without the query, which can hold secrets, such as
            // the token of a login link.
            .make_span_with(|request: &axum::http::Request<axum::body::Body>| {
                tracing::info_span!("request", method = %request.method(), path = %request.uri().path())
            })
            .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
    );
    // Browsers trust the Content-Type of responses rather than guessing from content.
    let app = app.layer(SetResponseHeaderLayer::overriding(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ));
    // Not in dev mode, so that localhost is never pinned to HTTPS, and live
    // reload's inline script is not blocked.
    let app = if dev {
        app.layer(LiveReloadLayer::new())
    } else {
        app.layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(STRICT_TRANSPORT_SECURITY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
    };
    // Outermost, so that live reload can still inject into uncompressed HTML.
    // Images are already skipped by the default predicate, and so are fonts and
    // videos here; compressing videos would also break requests for parts.
    app.layer(
        CompressionLayer::new().compress_when(
            DefaultPredicate::new()
                .and(NotForContentType::const_new("font/woff2"))
                .and(NotForContentType::const_new("video/")),
        ),
    )
}

/// Sets `Cache-Control` for static files. Versioned URLs (`?v=<hash>`, see
/// `assets::url`) change with their content, so they are cached for a year;
/// other files for a day. Errors are not cached, and nothing is in dev mode.
async fn cache_static(State(dev): State<bool>, request: Request, next: Next) -> Response {
    let versioned = request
        .uri()
        .path()
        .strip_prefix("/static/")
        .zip(
            request
                .uri()
                .query()
                .and_then(|query| query.strip_prefix("v=")),
        )
        .is_some_and(|(name, version)| assets::is_current(name, version));
    let mut response = next.run(request).await;
    if response.status().is_success() {
        let value = if dev {
            "no-cache"
        } else if versioned {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=86400"
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    response
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Language {
    En,
    Fi,
}

#[derive(Default, Deserialize)]
struct LanguageQuery {
    lang: Option<String>,
}

impl LanguageQuery {
    /// A query string that does not parse, e.g. one with `lang` repeated, counts
    /// as having no `lang`, so the page falls back to the browser's language.
    fn or_default(query: Result<Query<Self>, QueryRejection>) -> Self {
        query.map(|Query(query)| query).unwrap_or_default()
    }
}

impl Language {
    /// A supported language by its code, e.g. "fi" or "FI".
    fn from_code(code: &str) -> Option<Self> {
        [Self::En, Self::Fi]
            .into_iter()
            .find(|language| code.eq_ignore_ascii_case(language.code()))
    }

    fn from_request(query: &LanguageQuery, headers: &HeaderMap) -> Self {
        match query.lang.as_deref() {
            Some(code) => Self::from_code(code).unwrap_or(Self::En),
            None => Self::from_accept_language(headers),
        }
    }

    fn from_accept_language(headers: &HeaderMap) -> Self {
        let Some(value) = headers.get(header::ACCEPT_LANGUAGE) else {
            return Self::En;
        };
        let Ok(value) = value.to_str() else {
            return Self::En;
        };

        value
            .split(',')
            .enumerate()
            .filter_map(|(position, entry)| {
                let mut parts = entry.trim().split(';');
                let range = parts.next()?.trim();
                let language = if range == "*" {
                    Self::En
                } else {
                    Self::from_code(range.split('-').next()?)?
                };
                let quality = parts
                    .find_map(|part| {
                        let (name, value) = part.trim().split_once('=')?;
                        name.trim()
                            .eq_ignore_ascii_case("q")
                            .then(|| value.trim().parse::<f32>().ok())
                            .flatten()
                    })
                    .unwrap_or(1.0);

                (quality > 0.0 && quality <= 1.0).then_some((language, quality, position))
            })
            .max_by(
                |(_, left_quality, left_position), (_, right_quality, right_position)| {
                    left_quality
                        .total_cmp(right_quality)
                        .then_with(|| right_position.cmp(left_position))
                },
            )
            .map_or(Self::En, |(language, _, _)| language)
    }

    fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Fi => "fi",
        }
    }

    /// Open Graph locale, e.g. for link previews.
    fn locale(self) -> &'static str {
        match self {
            Self::En => "en_US",
            Self::Fi => "fi_FI",
        }
    }
}

struct Translations {
    site_title: &'static str,
    description: &'static str,
    quote_first_line: &'static str,
    quote_second_line: &'static str,
    quote_attribution: &'static str,
    language_label: &'static str,
    who: &'static str,
    bio: &'static str,
    blog_link: &'static str,
    tai_chi: &'static str,
    teachers: &'static str,
    rami_link: &'static str,
    association_link: &'static str,
    master_link: &'static str,
    what: &'static str,
    classes: &'static str,
    #[expect(dead_code, reason = "price is hidden on the page for now")]
    price: &'static str,
    booking: &'static str,
    email_link: &'static str,
    email_subject: &'static str,
    profile_alt: &'static str,
    not_found_title: &'static str,
    not_found: &'static str,
    front_page_link: &'static str,
    business_id_label: &'static str,
    lessons_title: &'static str,
    lesson_number: &'static str,
    movement: &'static str,
    movements: &'static str,
    free: &'static str,
    passing_label: &'static str,
    passing_practice: &'static str,
    passing_quiz: &'static str,
    passing_review: &'static str,
    previous_lesson: &'static str,
    next_lesson: &'static str,
    all_lessons: &'static str,
    login_title: &'static str,
    register_title: &'static str,
    email_label: &'static str,
    login_intro: &'static str,
    register_intro: &'static str,
    send_link: &'static str,
    no_account: &'static str,
    have_account: &'static str,
    link_sent_title: &'static str,
    link_sent_login: &'static str,
    link_sent_register: &'static str,
    invalid_email: &'static str,
    send_failed: &'static str,
    confirm_intro: &'static str,
    link_expired: &'static str,
    new_link: &'static str,
    account_title: &'static str,
    logged_in_as: &'static str,
    member_until: &'static str,
    not_member: &'static str,
    logout: &'static str,
    delete_title: &'static str,
    delete_info: &'static str,
    delete_confirm: &'static str,
    account_deleted: &'static str,
    done: &'static str,
    locked: &'static str,
    needs_login: &'static str,
    login_inline: &'static str,
    register_inline: &'static str,
    needs_membership: &'static str,
    needs_previous: &'static str,
    mark_done: &'static str,
    lesson_done: &'static str,
    server_error_title: &'static str,
    server_error: &'static str,
    video_notice: &'static str,
    video_unsupported: &'static str,
    lesson_count: &'static str,
    practice_count: &'static str,
    membership_title: &'static str,
    membership_intro: &'static str,
    join_monthly: &'static str,
    join_yearly: &'static str,
    manage_membership: &'static str,
    welcome_title: &'static str,
    membership_active: &'static str,
    membership_pending: &'static str,
    payment_unavailable: &'static str,
    join_link: &'static str,
    to_lessons: &'static str,
    terms_title: &'static str,
    privacy_title: &'static str,
    accept_terms: &'static str,
    terms_link: &'static str,
    privacy_link: &'static str,
    accept_terms_needed: &'static str,
    withdrawal_consent: &'static str,
    set_progress: &'static str,
    set_finished: &'static str,
    needs_set: &'static str,
    needs_lesson: &'static str,
}

impl Translations {
    #[expect(clippy::too_many_lines, reason = "every text in both languages")]
    fn for_language(language: Language) -> Self {
        match language {
            Language::En => Self {
                site_title: "Online Tai Chi Classes",
                description: "Private online Tai Chi classes for beginners and fellow Tung-style practitioners. Book a free introductory session with Timo.",
                quote_first_line: "Practice Tai Chi.",
                quote_second_line: "It's good for you.",
                quote_attribution: "Master Tung Chen Wei",
                language_label: "Language",
                who: "Who am I?",
                bio: "My name is Timo, and {tai_chi} is my passion. I write {blog} about my own experiences with the art.",
                blog_link: "a blog",
                tai_chi: "Tai Chi",
                teachers: "{rami} is my teacher from {association}, and my master is {master}.",
                rami_link: "Rami Salo",
                association_link: "Tai Chi Chuan ry",
                master_link: "Tung Chen Wei",
                what: "What do I do?",
                classes: "I offer private online classes to beginners and fellow Tung-style practitioners. My classes are flexible, and we can personalize them to suit your needs. You only need enough space to practice and make a video call at the same time.",
                price: "One class is one hour long. Classes are 30 € each or 100 € for a package of four.",
                booking: "Send me {email} to schedule a free introductory session!",
                email_link: "an email",
                email_subject: "Interest in Tai Chi Classes",
                profile_alt: "Timo practicing Tai Chi in a garden",
                not_found_title: "Not Found",
                not_found: "The page you are looking for does not exist. Try {front_page} instead.",
                front_page_link: "front page",
                business_id_label: "Business ID",
                lessons_title: "Lessons",
                lesson_number: "Lesson {number}",
                movement: "Movement",
                movements: "Movements",
                free: "Free",
                passing_label: "To pass",
                passing_practice: "practise and mark it done",
                passing_quiz: "answer a quiz",
                passing_review: "send a video for review",
                previous_lesson: "Previous lesson",
                next_lesson: "Next lesson",
                all_lessons: "All sets",
                login_title: "Log in",
                register_title: "Register",
                email_label: "Email address",
                login_intro: "Enter your email address, and I'll send you a link to log in.",
                register_intro: "Register with your email address to start the lessons. The first lessons are free.",
                send_link: "Send the link",
                no_account: "No account yet? {register}.",
                have_account: "Already registered? {login}.",
                link_sent_title: "Check your email",
                link_sent_login: "If there is an account for this address, a link to log in is on its way to it. Look in the spam folder too if it doesn't arrive.",
                link_sent_register: "A link to finish registering is on its way to you. Look in the spam folder too if it doesn't arrive.",
                invalid_email: "Please check the email address.",
                send_failed: "The email could not be sent. Please try again in a while.",
                confirm_intro: "Press the button to log in.",
                link_expired: "This link has expired or has already been used. {login}.",
                new_link: "Ask for a new link",
                account_title: "Account",
                logged_in_as: "Logged in as",
                member_until: "Membership paid until {date}.",
                not_member: "No membership yet: the free lessons are open to you.",
                logout: "Log out",
                delete_title: "Delete account",
                delete_info: "Deleting your account removes your email address and your progress for good, and ends your membership and its payments.",
                delete_confirm: "I want to delete my account",
                account_deleted: "Your account has been deleted.",
                done: "Passed",
                locked: "Locked",
                needs_login: "{login} or {register} to start the lessons.",
                login_inline: "Log in",
                register_inline: "register",
                needs_membership: "This lesson is for members. {join}.",
                needs_previous: "This lesson opens when you have passed the one before it.",
                mark_done: "Mark as practised",
                lesson_done: "You have passed this lesson.",
                server_error_title: "Something went wrong",
                server_error: "Please try again in a while.",
                video_notice: "The videos are for your own practice only. Downloading or sharing them is against the terms of use.",
                video_unsupported: "Your browser can't play this video.",
                lesson_count: "{count} lessons",
                practice_count: "{count} practices",
                membership_title: "Membership",
                membership_intro: "A membership opens the lessons in order, the practices and, as you go, the following sets.",
                join_monthly: "Join for {price} a month",
                join_yearly: "Join for {price} a year",
                manage_membership: "Manage membership and payments",
                welcome_title: "Thank you!",
                membership_active: "Your membership is active. Welcome!",
                membership_pending: "Your payment is being processed. The membership starts as soon as it is through.",
                payment_unavailable: "The payment service can't be reached right now. Please try again in a while.",
                join_link: "Become a member",
                to_lessons: "On to the lessons",
                terms_title: "Terms of use",
                privacy_title: "Privacy notice",
                accept_terms: "I accept the {terms} and have read the {privacy}.",
                terms_link: "terms of use",
                privacy_link: "privacy notice",
                accept_terms_needed: "Please accept the terms of use.",
                withdrawal_consent: "I want my membership to start right away, and I understand that I then lose my right of withdrawal.",
                set_progress: "{passed} of {total} lessons passed",
                set_finished: "Finished",
                needs_set: "Opens when {set} is finished.",
                needs_lesson: "Opens when you have passed {lesson}.",
            },
            Language::Fi => Self {
                site_title: "Taijiopetusta",
                description: "Yksityisiä taijin verkkotunteja aloittelijoille ja Tung-tyylin harjoittelijoille. Varaa maksuton tutustumistunti Timon kanssa.",
                quote_first_line: "Harjoittele taijia.",
                quote_second_line: "Se on hyväksi sinulle.",
                quote_attribution: "Mestari Tung Chen Wei",
                language_label: "Kieli",
                who: "Kuka olen?",
                bio: "Nimeni on Timo, ja intohimoni on {tai_chi}. Kirjoitan {blog} omista kokemuksistani lajin tiimoilta.",
                blog_link: "blogia",
                tai_chi: "taiji",
                teachers: "{rami} on opettajani {association}:stä, ja mestarini on {master}.",
                rami_link: "Rami Salo",
                association_link: "Tai Chi Chuan ry",
                master_link: "Tung Chen Wei",
                what: "Mitä teen?",
                classes: "Tarjoan yksityisiä verkkotunteja aloittelijoille ja muille Tung-tyylin harjoittelijoille. Oppituntini ovat joustavia, ja voimme mukauttaa niitä omiin tarpeisiisi. Tarvitset vain riittävästi tilaa harjoitteluun ja mahdollisuuden videopuheluun samanaikaisesti.",
                price: "Yksi oppitunti on yhden tunnin pituinen. Oppitunti maksaa 30 € tai neljän oppitunnin paketti 100 €.",
                booking: "Varaa maksuton tutustumisvideopuhelu lähettämällä minulle {email}!",
                email_link: "sähköpostia",
                email_subject: "Kiinnostusta taijiopetuksesta",
                profile_alt: "Timo harjoittelee taijia puutarhassa",
                not_found_title: "Sivua ei löydy",
                not_found: "Etsimääsi sivua ei ole olemassa. Kokeile {front_page}.",
                front_page_link: "etusivua",
                business_id_label: "Y-tunnus",
                lessons_title: "Oppitunnit",
                lesson_number: "Oppitunti {number}",
                movement: "Liike",
                movements: "Liikkeet",
                free: "Maksuton",
                passing_label: "Läpäisy",
                passing_practice: "harjoittele ja merkitse tehdyksi",
                passing_quiz: "vastaa kysymyksiin",
                passing_review: "lähetä video arvioitavaksi",
                previous_lesson: "Edellinen oppitunti",
                next_lesson: "Seuraava oppitunti",
                all_lessons: "Kaikki kokonaisuudet",
                login_title: "Kirjaudu sisään",
                register_title: "Rekisteröidy",
                email_label: "Sähköpostiosoite",
                login_intro: "Anna sähköpostiosoitteesi, niin lähetän sinulle linkin, jolla kirjaudut sisään.",
                register_intro: "Rekisteröidy sähköpostiosoitteellasi aloittaaksesi oppitunnit. Ensimmäiset oppitunnit ovat maksuttomia.",
                send_link: "Lähetä linkki",
                no_account: "Eikö sinulla ole vielä tiliä? {register}.",
                have_account: "Oletko jo rekisteröitynyt? {login}.",
                link_sent_title: "Tarkista sähköpostisi",
                link_sent_login: "Jos tällä osoitteella on tili, kirjautumislinkki on matkalla siihen. Katso myös roskapostikansio, jos viestiä ei kuulu.",
                link_sent_register: "Linkki rekisteröitymisen viimeistelyyn on matkalla sinulle. Katso myös roskapostikansio, jos viestiä ei kuulu.",
                invalid_email: "Tarkista sähköpostiosoite.",
                send_failed: "Sähköpostin lähetys epäonnistui. Yritä hetken päästä uudelleen.",
                confirm_intro: "Kirjaudu sisään painamalla painiketta.",
                link_expired: "Tämä linkki on vanhentunut tai jo käytetty. {login}.",
                new_link: "Pyydä uusi linkki",
                account_title: "Tili",
                logged_in_as: "Kirjautuneena",
                member_until: "Jäsenyys maksettu {date} asti.",
                not_member: "Ei jäsenyyttä vielä: maksuttomat oppitunnit ovat avoinna.",
                logout: "Kirjaudu ulos",
                delete_title: "Poista tili",
                delete_info: "Tilin poistaminen poistaa sähköpostiosoitteesi ja edistymisesi pysyvästi sekä päättää jäsenyytesi ja sen maksut.",
                delete_confirm: "Haluan poistaa tilini",
                account_deleted: "Tilisi on poistettu.",
                done: "Läpäisty",
                locked: "Lukittu",
                needs_login: "{login} tai {register} aloittaaksesi oppitunnit.",
                login_inline: "Kirjaudu sisään",
                register_inline: "rekisteröidy",
                needs_membership: "Tämä oppitunti on jäsenille. {join}.",
                needs_previous: "Tämä oppitunti avautuu, kun olet läpäissyt sitä edeltävän.",
                mark_done: "Merkitse harjoitelluksi",
                lesson_done: "Olet läpäissyt tämän oppitunnin.",
                server_error_title: "Jokin meni vikaan",
                server_error: "Yritä hetken päästä uudelleen.",
                video_notice: "Videot ovat vain omaa harjoitteluasi varten. Niiden lataaminen tai jakaminen on käyttöehtojen vastaista.",
                video_unsupported: "Selaimesi ei pysty toistamaan tätä videota.",
                lesson_count: "{count} oppituntia",
                practice_count: "{count} harjoitetta",
                membership_title: "Jäsenyys",
                membership_intro: "Jäsenyys avaa oppitunnit järjestyksessä, harjoitteet ja edetessäsi seuraavat kokonaisuudet.",
                join_monthly: "Liity: {price} kuukaudessa",
                join_yearly: "Liity: {price} vuodessa",
                manage_membership: "Hallitse jäsenyyttä ja maksuja",
                welcome_title: "Kiitos!",
                membership_active: "Jäsenyytesi on voimassa. Tervetuloa!",
                membership_pending: "Maksuasi käsitellään. Jäsenyys alkaa heti, kun maksu on mennyt läpi.",
                payment_unavailable: "Maksupalveluun ei juuri nyt saada yhteyttä. Yritä hetken päästä uudelleen.",
                join_link: "Liity jäseneksi",
                to_lessons: "Oppitunneille",
                terms_title: "Käyttöehdot",
                privacy_title: "Tietosuojaseloste",
                accept_terms: "Hyväksyn {terms} ja olen lukenut {privacy}.",
                terms_link: "käyttöehdot",
                privacy_link: "tietosuojaselosteen",
                accept_terms_needed: "Hyväksy käyttöehdot.",
                withdrawal_consent: "Haluan jäsenyyteni alkavan heti ja ymmärrän, että menetän silloin peruuttamisoikeuteni.",
                set_progress: "{passed}/{total} oppituntia läpäisty",
                set_finished: "Suoritettu",
                needs_set: "Avautuu, kun {set} on suoritettu.",
                needs_lesson: "Avautuu, kun {lesson} on läpäisty.",
            },
        }
    }
}

enum Segment<'a> {
    Text(&'a str),
    Link { href: &'a str, label: &'a str },
}

/// Render translated text with a closed set of named links; all text and attributes are Maud-escaped.
fn linked_markup<'a>(template: &'a str, links: &'a [(&'a str, &'a str, &'a str)]) -> Markup {
    let mut segments = Vec::new();
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        let Some(end_offset) = rest[start + 1..].find('}') else {
            break;
        };
        let end = start + 1 + end_offset;

        if start > 0 {
            segments.push(Segment::Text(&rest[..start]));
        }

        let key = &rest[start + 1..end];
        if let Some((_, href, label)) = links.iter().find(|(name, _, _)| *name == key) {
            segments.push(Segment::Link { href, label });
        } else {
            segments.push(Segment::Text(&rest[start..=end]));
        }

        rest = &rest[end + 1..];
    }

    if !rest.is_empty() {
        segments.push(Segment::Text(rest));
    }

    html! {
        @for segment in segments {
            @match segment {
                Segment::Text(text) => (text),
                Segment::Link { href, label } => a href=(href) { (label) },
            }
        }
    }
}

/// Latin subsets of the fonts used above the fold, see `@font-face` in styles.css.
const PRELOADED_FONTS: &[&str] = &[
    "/static/fonts/nunito-latin.woff2",
    "/static/fonts/electrolize-latin.woff2",
    "/static/fonts/gloria-hallelujah-latin.woff2",
];

/// Seconds the logo takes to turn once; the same as `.logo`'s animation in
/// styles.css, which has a `phase-<n>` class for each second of the turn.
const LOGO_TURN_SECONDS: u64 = 62;

/// The class starting the logo's turn where the clock has it now, so it keeps
/// turning from page to page instead of starting over on each.
fn logo_phase(now: std::time::SystemTime) -> String {
    let millis = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let seconds = (millis + 500) / 1000 % u128::from(LOGO_TURN_SECONDS);
    format!("logo phase-{seconds}")
}

/// `path` is the page's own path, for the links to it in the other languages.
fn layout(
    language: Language,
    t: &Translations,
    path: &str,
    page_title: &str,
    head: &Markup,
    content: &Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang=(language.code()) {
            head {
                meta charset="utf-8";
                title { (page_title) " ☯ Taiji-Timo" }
                meta name="viewport" content="width=device-width, initial-scale=1";
                link href=(assets::url("styles.css")) rel="stylesheet";
                // The .ico for browsers without SVG icons; sizes keeps others from preferring it.
                link rel="icon" href="/favicon.ico" sizes="32x32";
                link rel="icon" href=(assets::url("favicon.svg")) type="image/svg+xml";
                link rel="apple-touch-icon" href=(assets::url("apple-touch-icon.png"));
                // Fetch the first screen's fonts together with the stylesheet instead of
                // after it. The URLs must match what styles.css resolves to.
                @for font in PRELOADED_FONTS {
                    link rel="preload" href=(font) as="font" type="font/woff2" crossorigin;
                }
                (head)
            }
            body {
                nav aria-label=(t.language_label) {
                    a href={ (path) "?lang=en" } lang="en" aria-current=[(language == Language::En).then_some("page")] { "In English" }
                    " · "
                    a href={ (path) "?lang=fi" } lang="fi" aria-current=[(language == Language::Fi).then_some("page")] { "suomeksi" }
                }
                header {
                    // The same file is the favicon, so the logo is kept in one place.
                    div class=(logo_phase(std::time::SystemTime::now())) { (PreEscaped(include_str!("../dist/favicon.svg"))) }
                    h1 { (t.site_title) }
                    figure.quote {
                        blockquote {
                            span.keep-together { (t.quote_first_line) }
                            " "
                            span.keep-together { (t.quote_second_line) }
                        }
                        figcaption { "- " (t.quote_attribution) }
                    }
                }
                main {
                    (content)
                }
                footer {
                    p { "Taiji-Timo · " (t.business_id_label) " 2212553-3" }
                }
            }
        }
    }
}

async fn index(
    State(site_url): State<Arc<str>>,
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let language = Language::from_request(&LanguageQuery::or_default(query), &headers);
    let t = Translations::for_language(language);
    let bio_links = [
        (
            "tai_chi",
            "https://en.wikipedia.org/wiki/Tai_chi",
            t.tai_chi,
        ),
        ("blog", "https://www.taichiwayfarer.com", t.blog_link),
    ];
    let teacher_links = [
        (
            "rami",
            "https://www.facebook.com/taichichuanboscofalconeria/",
            t.rami_link,
        ),
        ("association", "https://taichichuan.fi", t.association_link),
        ("master", "http://tungtaichi.com", t.master_link),
    ];
    let email_subject = urlencoding::encode(t.email_subject);
    let mailto = format!("mailto:taichiwayfarer@gmail.com?subject={email_subject}");
    let booking_links = [("email", mailto.as_str(), t.email_link)];
    let page_url = |language: Language| format!("{site_url}/?lang={}", language.code());

    (
        [(header::VARY, "Accept-Language")],
        layout(
            language,
            &t,
            "/",
            t.site_title,
            // For search engines and link previews. Each language has its own URL;
            // plain / picks one from the browser's languages.
            &html! {
                meta name="description" content=(t.description);
                link rel="canonical" href=(page_url(language));
                @for alternate in [Language::En, Language::Fi] {
                    link rel="alternate" hreflang=(alternate.code()) href=(page_url(alternate));
                }
                link rel="alternate" hreflang="x-default" href={ (site_url) "/" };
                meta property="og:type" content="website";
                meta property="og:site_name" content="Taiji-Timo";
                meta property="og:title" content=(t.site_title);
                meta property="og:description" content=(t.description);
                meta property="og:url" content=(page_url(language));
                meta property="og:locale" content=(language.locale());
                // The largest crop in the repository; previews show larger images better.
                meta property="og:image" content={ (site_url) (assets::url("img/profile-432.jpg")) };
                meta property="og:image:width" content="432";
                meta property="og:image:height" content="432";
                meta property="og:image:alt" content=(t.profile_alt);
            },
            &html! {
                h2 { (t.who) }
                // Shown 9rem (144 CSS px) wide: 288 px for 1x and 2x screens, 432 px for 3x.
                img .profile
                    src=(assets::url("img/profile.jpg"))
                    srcset={ (assets::url("img/profile.jpg")) " 2x, " (assets::url("img/profile-432.jpg")) " 3x" }
                    width="288" height="288" alt=(t.profile_alt);
                p { (linked_markup(t.bio, &bio_links)) }
                p { (linked_markup(t.teachers, &teacher_links)) }
                h2 { (t.what) }
                p { (t.classes) }
                //p { (t.price) }
                p { (linked_markup(t.booking, &booking_links)) }
            },
        ),
    )
}

async fn handler_404(
    query: Result<Query<LanguageQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Response {
    not_found(Language::from_request(
        &LanguageQuery::or_default(query),
        &headers,
    ))
}

fn not_found(language: Language) -> Response {
    let t = Translations::for_language(language);
    let front_page_href = format!("/?lang={}", language.code());
    let front_page_links = [("front_page", front_page_href.as_str(), t.front_page_link)];

    (
        StatusCode::NOT_FOUND,
        [(header::VARY, "Accept-Language")],
        layout(
            language,
            &t,
            "/",
            t.not_found_title,
            &html! {},
            &html! {
                h2 { (t.not_found_title) }
                p { (linked_markup(t.not_found, &front_page_links)) }
            },
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::HeaderValue, http::Request};
    use tower::ServiceExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// With a trailing slash, which `app` removes.
    const TEST_SITE_URL: &str = "https://example.test/";

    fn accept_language(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static(value));
        headers
    }

    fn query(lang: Option<&str>) -> LanguageQuery {
        LanguageQuery {
            lang: lang.map(str::to_string),
        }
    }

    #[test]
    fn logo_starts_where_the_clock_has_it() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |millis| logo_phase(UNIX_EPOCH + Duration::from_millis(millis));
        assert_eq!(at(0), "logo phase-0");
        assert_eq!(at(1_499), "logo phase-1");
        assert_eq!(at(1_500), "logo phase-2");
        assert_eq!(at(61_600), "logo phase-0");
        assert_eq!(at(62_000 * 1_000 + 30_000), "logo phase-30");
    }

    #[test]
    fn styles_have_each_logo_phase() {
        let styles = include_str!("../dist/styles.css");
        assert!(styles.contains(&format!(
            "animation: spin {LOGO_TURN_SECONDS}s linear infinite"
        )));
        for second in 1..LOGO_TURN_SECONDS {
            let rule = format!(".logo.phase-{second} {{ animation-delay: -{second}s; }}");
            assert!(styles.contains(&rule), "{rule}");
        }
    }

    fn assert_accept_language(value: &'static str, expected: Language) {
        assert!(
            Language::from_accept_language(&accept_language(value)) == expected,
            "Accept-Language {value:?} should resolve to {:?}",
            expected.code()
        );
    }

    #[test]
    fn accept_language_missing_defaults_to_english() {
        assert_eq!(
            Language::from_accept_language(&HeaderMap::new()),
            Language::En
        );
    }

    #[test]
    fn accept_language_picks_supported_language() {
        assert_accept_language("fi", Language::Fi);
        assert_accept_language("en", Language::En);
        assert_accept_language("FI", Language::Fi);
        assert_accept_language("fi-FI", Language::Fi);
        assert_accept_language("fi-FI,fi;q=0.9,en;q=0.8", Language::Fi);
        assert_accept_language("en-US,en;q=0.9,fi;q=0.8", Language::En);
    }

    #[test]
    fn accept_language_prefers_highest_quality() {
        assert_accept_language("en;q=0.5, fi;q=0.8", Language::Fi);
        assert_accept_language("fi;q=0.3, en;q=0.4", Language::En);
        assert_accept_language("fi ; q = 0.9 , en ; q = 0.1", Language::Fi);
    }

    #[test]
    fn accept_language_breaks_ties_by_position() {
        assert_accept_language("fi;q=0.8, en;q=0.8", Language::Fi);
        assert_accept_language("en, fi", Language::En);
    }

    #[test]
    fn accept_language_skips_unsupported_languages() {
        assert_accept_language("sv,fi;q=0.5", Language::Fi);
        assert_accept_language("de,sv", Language::En);
    }

    #[test]
    fn accept_language_ignores_out_of_range_quality() {
        assert_accept_language("fi;q=0", Language::En);
        assert_accept_language("fi;q=0,en", Language::En);
        assert_accept_language("fi;q=2", Language::En);
        assert_accept_language("fi;q=-1", Language::En);
    }

    #[test]
    fn accept_language_wildcard_resolves_to_english() {
        assert_accept_language("*", Language::En);
        assert_accept_language("*, fi;q=0.5", Language::En);
        assert_accept_language("*;q=0.1, fi", Language::Fi);
    }

    #[test]
    fn accept_language_treats_unparsable_quality_as_one() {
        assert_accept_language("en;q=0.9, fi;q=abc", Language::Fi);
    }

    #[test]
    fn accept_language_non_ascii_header_defaults_to_english() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_bytes(b"fi\xff")?);
        assert_eq!(Language::from_accept_language(&headers), Language::En);
        Ok(())
    }

    #[test]
    fn flag_is_enabled_when_set_and_not_false() {
        assert!(flag_enabled(Some("")));
        assert!(flag_enabled(Some("1")));
        assert!(flag_enabled(Some("true")));
        assert!(!flag_enabled(None));
        assert!(!flag_enabled(Some("0")));
        assert!(!flag_enabled(Some("false")));
    }

    #[test]
    fn language_from_code_round_trips_and_ignores_case() {
        for language in [Language::En, Language::Fi] {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
        assert_eq!(Language::from_code("FI"), Some(Language::Fi));
        assert!(Language::from_code("sv").is_none());
        assert!(Language::from_code("").is_none());
        assert!(Language::from_code("*").is_none());
    }

    #[test]
    fn query_parameter_overrides_accept_language() {
        let finnish = accept_language("fi");
        let english = accept_language("en");
        assert_eq!(
            Language::from_request(&query(Some("en")), &finnish),
            Language::En
        );
        assert_eq!(
            Language::from_request(&query(Some("fi")), &english),
            Language::Fi
        );
        assert_eq!(
            Language::from_request(&query(Some("FI")), &english),
            Language::Fi
        );
        assert_eq!(Language::from_request(&query(None), &finnish), Language::Fi);
    }

    #[test]
    fn unknown_query_language_falls_back_to_english_not_header() {
        let finnish = accept_language("fi");
        assert_eq!(
            Language::from_request(&query(Some("sv")), &finnish),
            Language::En
        );
        assert_eq!(
            Language::from_request(&query(Some("")), &finnish),
            Language::En
        );
    }

    #[test]
    fn linked_markup_replaces_placeholders_with_links() {
        let links = [("a", "https://a.example", "A"), ("b", "/b", "B")];
        let markup = linked_markup("x {a} y {b}{a} z", &links);
        assert_eq!(
            markup.into_string(),
            r#"x <a href="https://a.example">A</a> y <a href="/b">B</a><a href="https://a.example">A</a> z"#
        );
    }

    #[test]
    fn linked_markup_without_placeholders_is_plain_text() {
        assert_eq!(linked_markup("plain", &[]).into_string(), "plain");
        assert_eq!(linked_markup("", &[]).into_string(), "");
    }

    #[test]
    fn linked_markup_keeps_unknown_and_unclosed_placeholders() {
        let links = [("a", "/a", "A")];
        assert_eq!(
            linked_markup("{missing} {a}", &links).into_string(),
            r#"{missing} <a href="/a">A</a>"#
        );
        assert_eq!(
            linked_markup("{a} {open", &links).into_string(),
            r#"<a href="/a">A</a> {open"#
        );
    }

    #[test]
    fn linked_markup_escapes_text_href_and_label() {
        let links = [("a", r#"/?x="1"&y"#, "<b>")];
        assert_eq!(
            linked_markup("<i>{a}</i>", &links).into_string(),
            r#"&lt;i&gt;<a href="/?x=&quot;1&quot;&amp;y">&lt;b&gt;</a>&lt;/i&gt;"#
        );
    }

    async fn get(
        uri: &str,
        accept_language: Option<&'static str>,
    ) -> Result<(StatusCode, HeaderMap, String), Box<dyn std::error::Error>> {
        get_from(
            app(Path::new("dist"), false, TEST_SITE_URL, None),
            uri,
            accept_language,
        )
        .await
    }

    async fn get_from(
        app: Router,
        uri: &str,
        accept_language: Option<&'static str>,
    ) -> Result<(StatusCode, HeaderMap, String), Box<dyn std::error::Error>> {
        let mut request = Request::builder().uri(uri);
        if let Some(value) = accept_language {
            request = request.header(header::ACCEPT_LANGUAGE, value);
        }
        let response = app.oneshot(request.body(Body::empty())?).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, headers, String::from_utf8_lossy(&body).into_owned()))
    }

    #[tokio::test]
    async fn index_renders_english_by_default() -> TestResult {
        let (status, headers, body) = get("/", None).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::VARY], "Accept-Language");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()?
                .starts_with("text/html")
        );
        assert!(body.starts_with("<!DOCTYPE html>"));
        assert!(body.contains(r#"<head><meta charset="utf-8"><title>"#));
        assert!(!body.contains("X-UA-Compatible"));
        assert!(
            body.contains(
                r#" width="288" height="288" alt="Timo practicing Tai Chi in a garden">"#
            )
        );
        assert!(
            body.contains("</blockquote><figcaption>- Master Tung Chen Wei</figcaption></figure>")
        );
        assert!(body.contains(r#"<html lang="en">"#));
        assert!(body.contains("<title>Online Tai Chi Classes ☯ Taiji-Timo</title>"));
        assert!(body.contains("<footer><p>Taiji-Timo · Business ID 2212553-3</p></footer>"));
        assert!(
            !body.contains("role='banner'"),
            "<header> is already the banner landmark"
        );
        assert!(body.contains("<div class=\"logo phase-"));
        assert!(body.contains(
            "\"><svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 360 360' aria-hidden='true'"
        ));
        assert!(
            !body.contains("fonts.googleapis.com") && !body.contains("fonts.gstatic.com"),
            "fonts are self-hosted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn index_renders_finnish_from_query_or_header() -> TestResult {
        for (uri, accept) in [("/?lang=fi", None), ("/", Some("fi-FI,fi;q=0.9"))] {
            let (status, _, body) = get(uri, accept).await?;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains(r#"<html lang="fi">"#), "{uri} {accept:?}");
            assert!(body.contains("Taijiopetusta"), "{uri} {accept:?}");
            assert!(
                body.contains("<footer><p>Taiji-Timo · Y-tunnus 2212553-3</p></footer>"),
                "{uri} {accept:?}"
            );
            assert!(
                body.contains(r#"alt="Timo harjoittelee taijia puutarhassa">"#),
                "{uri} {accept:?}"
            );
            assert!(
                body.contains("<figcaption>- Mestari Tung Chen Wei</figcaption>"),
                "{uri} {accept:?}"
            );
            assert!(
                body.contains(r#"<a href="/?lang=fi" lang="fi" aria-current="page">"#),
                "{uri} {accept:?}"
            );
            assert!(
                body.contains(r#"<a href="/?lang=en" lang="en">"#),
                "{uri} {accept:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn index_resolves_every_placeholder() -> TestResult {
        for lang in ["en", "fi"] {
            let (_, _, body) = get(&format!("/?lang={lang}"), None).await?;
            assert!(!body.contains('{'), "unresolved placeholder in {lang} page");
        }
        Ok(())
    }

    #[tokio::test]
    async fn index_encodes_email_subject() -> TestResult {
        let (_, _, body) = get("/?lang=fi", None).await?;
        assert!(
            body.contains(
                "mailto:taichiwayfarer@gmail.com?subject=Kiinnostusta%20taijiopetuksesta"
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn unparsable_query_falls_back_to_accept_language() -> TestResult {
        for (uri, status) in [
            ("/?lang=en&lang=en", StatusCode::OK),
            ("/does-not-exist?lang=en&lang=en", StatusCode::NOT_FOUND),
        ] {
            let (actual, headers, body) = get(uri, Some("fi")).await?;
            assert_eq!(actual, status, "{uri}");
            assert!(
                headers[header::CONTENT_TYPE]
                    .to_str()?
                    .starts_with("text/html"),
                "{uri}"
            );
            assert!(body.contains(r#"<html lang="fi">"#), "{uri}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn index_head_describes_page_for_search_and_sharing() -> TestResult {
        for (lang, description) in [
            ("en", "Private online Tai Chi classes"),
            ("fi", "Yksityisiä taijin verkkotunteja"),
        ] {
            let (_, _, body) = get(&format!("/?lang={lang}"), None).await?;
            let url = format!("https://example.test/?lang={lang}");
            for tag in [
                format!(r#"<meta name="description" content="{description}"#),
                format!(r#"<link rel="canonical" href="{url}">"#),
                format!(r#"<meta property="og:url" content="{url}">"#),
                format!(r#"<meta property="og:description" content="{description}"#),
                r#"<link rel="alternate" hreflang="en" href="https://example.test/?lang=en">"#
                    .to_string(),
                r#"<link rel="alternate" hreflang="fi" href="https://example.test/?lang=fi">"#
                    .to_string(),
                r#"<link rel="alternate" hreflang="x-default" href="https://example.test/">"#
                    .to_string(),
                format!(
                    r#"<meta property="og:image" content="https://example.test{}">"#,
                    assets::url("img/profile-432.jpg")
                ),
                r#"<meta property="og:image:width" content="432">"#.to_string(),
                r#"<meta property="og:image:height" content="432">"#.to_string(),
            ] {
                assert!(body.contains(&tag), "{lang}: missing {tag}");
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn not_found_page_has_no_search_or_sharing_tags() -> TestResult {
        let (_, _, body) = get("/does-not-exist", None).await?;
        for marker in ["canonical", "hreflang", "og:", r#"name="description""#] {
            assert!(!body.contains(marker), "{marker}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn unknown_path_renders_translated_404() -> TestResult {
        let (status, headers, body) = get("/does-not-exist?lang=fi", None).await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[header::VARY], "Accept-Language");
        assert!(body.contains("<title>Sivua ei löydy ☯ Taiji-Timo</title>"));
        assert!(body.contains(r#"<a href="/?lang=fi">etusivua</a>"#));
        Ok(())
    }

    #[tokio::test]
    async fn static_files_are_served() -> TestResult {
        let (status, headers, _) = get("/static/styles.css", None).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()?
                .starts_with("text/css")
        );

        let (status, _, _) = get("/static/img/profile.jpg", None).await?;
        assert_eq!(status, StatusCode::OK);

        let (status, _, _) = get("/favicon.ico", None).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn preloaded_fonts_match_stylesheet() -> TestResult {
        let (_, _, body) = get("/", None).await?;
        let (_, _, css) = get("/static/styles.css", None).await?;
        for font in PRELOADED_FONTS {
            assert!(
                body.contains(&format!(
                    r#"<link rel="preload" href="{font}" as="font" type="font/woff2" crossorigin>"#
                )),
                "{font} not preloaded"
            );
            let relative = font.strip_prefix("/static/").unwrap_or(font);
            assert!(
                css.contains(&format!("url({relative})")),
                "{font} not in styles.css"
            );
            let (status, _, _) = get(font, None).await?;
            assert_eq!(status, StatusCode::OK, "{font}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn stylesheet_fonts_are_served() -> TestResult {
        let (_, _, css) = get("/static/styles.css", None).await?;
        let fonts: Vec<&str> = css
            .split("url(")
            .skip(1)
            .filter_map(|rest| rest.split_once(')').map(|(url, _)| url))
            .collect();
        assert_eq!(fonts.len(), 5, "{fonts:?}");
        for font in fonts {
            let (status, headers, _) = get(&format!("/static/{font}"), None).await?;
            assert_eq!(status, StatusCode::OK, "{font}");
            assert_eq!(headers[header::CONTENT_TYPE], "font/woff2", "{font}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn page_links_versioned_assets() -> TestResult {
        let (_, _, body) = get("/", None).await?;
        for name in [
            "styles.css",
            "img/profile.jpg",
            "img/profile-432.jpg",
            "favicon.svg",
            "apple-touch-icon.png",
        ] {
            let url = assets::url(name);
            assert!(url.contains("?v="), "{url}");
        }
        for name in [
            "styles.css",
            "img/profile.jpg",
            "favicon.svg",
            "apple-touch-icon.png",
        ] {
            let url = assets::url(name);
            assert!(body.contains(&format!(r#"="{url}""#)), "{url} not linked");
        }
        let srcset = format!(
            r#"srcset="{} 2x, {} 3x""#,
            assets::url("img/profile.jpg"),
            assets::url("img/profile-432.jpg")
        );
        assert!(body.contains(&srcset), "missing {srcset}");
        Ok(())
    }

    #[tokio::test]
    async fn versioned_static_files_are_cached_immutably() -> TestResult {
        for name in [
            "styles.css",
            "img/profile.jpg",
            "img/profile-432.jpg",
            "favicon.svg",
            "apple-touch-icon.png",
        ] {
            let (status, headers, _) = get(&assets::url(name), None).await?;
            assert_eq!(status, StatusCode::OK, "{name}");
            assert_eq!(
                headers[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable",
                "{name}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn unversioned_or_stale_static_files_are_cached_for_a_day() -> TestResult {
        for uri in [
            "/static/styles.css",
            "/static/styles.css?v=0000000000000000",
            "/static/fonts/nunito-latin.woff2",
            "/favicon.ico",
        ] {
            let (status, headers, _) = get(uri, None).await?;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(
                headers[header::CACHE_CONTROL],
                "public, max-age=86400",
                "{uri}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_static_files_render_translated_404() -> TestResult {
        for uri in [
            "/static/missing.css",
            "/static/",
            "/static/img/",
            "/static/../Cargo.toml",
        ] {
            let (status, headers, body) = get(uri, Some("fi")).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert!(
                headers[header::CONTENT_TYPE]
                    .to_str()?
                    .starts_with("text/html"),
                "{uri}"
            );
            assert!(
                body.contains("<title>Sivua ei löydy ☯ Taiji-Timo</title>"),
                "{uri}"
            );
        }
        Ok(())
    }

    /// The `Content-Encoding` and `Vary` of a response to `uri` for a browser accepting `encoding`.
    async fn encoding_of(
        app: Router,
        uri: &str,
        encoding: &'static str,
    ) -> Result<(Option<String>, String), Box<dyn std::error::Error>> {
        let request = Request::builder()
            .uri(uri)
            .header(header::ACCEPT_ENCODING, encoding)
            .body(Body::empty())?;
        let response = app.oneshot(request).await?;
        let content_encoding = response
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|value| value.to_str().map(str::to_string))
            .transpose()?;
        let vary = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .map(|value| value.to_str().map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        Ok((content_encoding, vary))
    }

    #[tokio::test]
    async fn text_responses_are_compressed() -> TestResult {
        for uri in ["/", "/?lang=fi", "/static/styles.css", "/does-not-exist"] {
            let (encoding, vary) = encoding_of(
                app(Path::new("dist"), false, TEST_SITE_URL, None),
                uri,
                "gzip, br",
            )
            .await?;
            assert_eq!(encoding.as_deref(), Some("br"), "{uri}");
            assert!(
                vary.to_lowercase().contains("accept-encoding"),
                "{uri}: {vary}"
            );
        }
        let (encoding, vary) = encoding_of(
            app(Path::new("dist"), false, TEST_SITE_URL, None),
            "/",
            "gzip",
        )
        .await?;
        assert_eq!(encoding.as_deref(), Some("gzip"));
        assert!(vary.contains("Accept-Language"), "{vary}");
        Ok(())
    }

    #[tokio::test]
    async fn fonts_and_images_are_not_compressed_again() -> TestResult {
        for uri in [
            "/static/fonts/nunito-latin.woff2",
            "/static/img/profile.jpg",
            "/static/apple-touch-icon.png",
        ] {
            let (encoding, _) = encoding_of(
                app(Path::new("dist"), false, TEST_SITE_URL, None),
                uri,
                "gzip, br",
            )
            .await?;
            assert_eq!(encoding, None, "{uri}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_static_files_and_pages_are_not_cached() -> TestResult {
        for uri in ["/static/missing.css", "/", "/does-not-exist"] {
            let (_, headers, _) = get(uri, None).await?;
            assert!(!headers.contains_key(header::CACHE_CONTROL), "{uri}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn dev_mode_disables_static_caching() -> TestResult {
        let (_, headers, _) = get_from(
            app(Path::new("dist"), true, TEST_SITE_URL, None),
            &assets::url("styles.css"),
            None,
        )
        .await?;
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        Ok(())
    }

    #[tokio::test]
    async fn responses_require_https_except_in_dev_mode() -> TestResult {
        for uri in ["/", "/static/styles.css", "/does-not-exist"] {
            let (_, headers, _) = get(uri, None).await?;
            assert_eq!(
                headers[header::STRICT_TRANSPORT_SECURITY],
                "max-age=31536000",
                "{uri}"
            );
            let (_, headers, _) =
                get_from(app(Path::new("dist"), true, TEST_SITE_URL, None), uri, None).await?;
            assert!(
                !headers.contains_key(header::STRICT_TRANSPORT_SECURITY),
                "{uri}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn responses_restrict_content_except_in_dev_mode() -> TestResult {
        for uri in ["/", "/static/styles.css", "/does-not-exist"] {
            let (_, headers, _) = get(uri, None).await?;
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{uri}");
            let policy = headers[header::CONTENT_SECURITY_POLICY].to_str()?;
            assert!(
                policy.starts_with("default-src 'none'; "),
                "{uri}: {policy}"
            );
            assert!(policy.contains("frame-ancestors 'none'"), "{uri}: {policy}");
            assert!(!policy.contains("  "), "{uri}: {policy}");

            let (_, headers, _) =
                get_from(app(Path::new("dist"), true, TEST_SITE_URL, None), uri, None).await?;
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{uri}");
            assert!(
                !headers.contains_key(header::CONTENT_SECURITY_POLICY),
                "{uri}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn pages_need_nothing_the_policy_blocks() -> TestResult {
        for uri in ["/", "/does-not-exist"] {
            let (_, _, body) = get(uri, None).await?;
            for blocked in ["<script", "<style", " style=", "<iframe", "<form", "<base"] {
                assert!(!body.contains(blocked), "{uri}: {blocked}");
            }
            // Images, the stylesheet, icons and fonts all come from the site
            // itself; links and the search engine tags may point anywhere.
            for tag in body.split('<').skip(1) {
                let tag = tag.split_once('>').map_or(tag, |(tag, _)| tag);
                if tag.starts_with("a ")
                    || tag.contains(r#"rel="canonical""#)
                    || tag.contains(r#"rel="alternate""#)
                {
                    continue;
                }
                for attribute in [r#" src=""#, r#" srcset=""#, r#" href=""#] {
                    let Some((_, value)) = tag.split_once(attribute) else {
                        continue;
                    };
                    let value = value.split_once('"').map_or(value, |(value, _)| value);
                    for url in value.split(", ") {
                        assert!(url.starts_with('/'), "{uri}: <{tag}>");
                    }
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn lessons_are_not_served_without_content() -> TestResult {
        for uri in ["/lessons", "/lessons/preparation"] {
            let (status, _, _) = get(uri, None).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        Ok(())
    }
}
