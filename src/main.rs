mod assets;

use axum::{
    Router,
    extract::{Query, Request, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Deserialize;
use std::path::Path;
use tower_http::{
    services::{ServeDir, ServeFile},
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
    let app = app(Path::new(&static_dir), dev);
    let app = if dev {
        app.layer(LiveReloadLayer::new())
    } else {
        app
    };
    // Behind a reverse proxy, set HOST=127.0.0.1 to accept local connections only.
    let host = std::env::var("HOST").unwrap_or("0.0.0.0".to_string());
    let port = std::env::var("PORT").unwrap_or("3000".to_string());
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

fn app(static_dir: &Path, dev: bool) -> Router {
    assets::init(static_dir);
    let static_files = Router::new()
        .nest_service("/static", ServeDir::new(static_dir))
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_dir.join("favicon.ico")),
        )
        .layer(middleware::from_fn_with_state(dev, cache_static));

    Router::new()
        .merge(static_files)
        .route("/", get(index))
        .fallback(handler_404)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
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

#[derive(Clone, Copy, PartialEq)]
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
}

struct Translations {
    site_title: &'static str,
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
}

impl Translations {
    fn for_language(language: Language) -> Self {
        match language {
            Language::En => Self {
                site_title: "Online Tai Chi Classes",
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
                profile_alt: "Profile picture",
                not_found_title: "Not Found",
                not_found: "The page you are looking for does not exist. Try {front_page} instead.",
                front_page_link: "front page",
            },
            Language::Fi => Self {
                site_title: "Taijiopetusta",
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
                profile_alt: "Profiilikuva",
                not_found_title: "Sivua ei löydy",
                not_found: "Etsimääsi sivua ei ole olemassa. Kokeile {front_page}.",
                front_page_link: "etusivua",
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

fn layout(language: Language, t: &Translations, page_title: &str, content: &Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang=(language.code()) {
            head {
                meta charset="utf-8";
                title { (page_title) " ☯ Taiji-Timo" }
                meta name="viewport" content="width=device-width, initial-scale=1";
                link href=(assets::url("styles.css")) rel="stylesheet";
                // Fetch the first screen's fonts together with the stylesheet instead of
                // after it. The URLs must match what styles.css resolves to.
                @for font in PRELOADED_FONTS {
                    link rel="preload" href=(font) as="font" type="font/woff2" crossorigin;
                }
            }
            body {
                nav aria-label=(t.language_label) {
                    a href="/?lang=en" lang="en" aria-current=[(language == Language::En).then_some("page")] { "In English" }
                    " · "
                    a href="/?lang=fi" lang="fi" aria-current=[(language == Language::Fi).then_some("page")] { "suomeksi" }
                }
                header {
                    (PreEscaped("
                    <div class='logo'><svg viewBox='0 0 360 360' aria-hidden='true' focusable='false'><circle fill='white' r='180' transform='translate(180, 180)'></circle><path transform='translate(180, 180) rotate(90)' d='M-180,0A180,180,0,1,1,180,0L0,0Z'></path><circle r='90' transform='translate(180, 270)'></circle><circle fill='white' r='90' transform='translate(180, 90)'></circle><circle fill='white' r='27' transform='translate(180, 270)'></circle><circle r='27' transform='translate(180, 90)'></circle><path stroke='black' stroke-width='2' fill='none' d='M276.714112750397,330.62330628061346A179,179,0,1,1,359,179.99999999999994'></path></svg></div>
                    "))
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
                    p { "Taiji-Timo © 2212553-3" }
                }
            }
        }
    }
}

async fn index(
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

    (
        [(header::VARY, "Accept-Language")],
        layout(
            language,
            &t,
            t.site_title,
            &html! {
                h2 { (t.who) }
                img .profile src=(assets::url("img/profile.jpg")) width="184" height="184" alt=(t.profile_alt);
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
) -> impl IntoResponse {
    let language = Language::from_request(&LanguageQuery::or_default(query), &headers);
    let t = Translations::for_language(language);
    let front_page_href = format!("/?lang={}", language.code());
    let front_page_links = [("front_page", front_page_href.as_str(), t.front_page_link)];

    (
        StatusCode::NOT_FOUND,
        [(header::VARY, "Accept-Language")],
        layout(
            language,
            &t,
            t.not_found_title,
            &html! {
                h2 { (t.not_found_title) }
                p { (linked_markup(t.not_found, &front_page_links)) }
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::HeaderValue, http::Request};
    use tower::ServiceExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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

    fn assert_accept_language(value: &'static str, expected: Language) {
        assert!(
            Language::from_accept_language(&accept_language(value)) == expected,
            "Accept-Language {value:?} should resolve to {:?}",
            expected.code()
        );
    }

    #[test]
    fn accept_language_missing_defaults_to_english() {
        assert!(Language::from_accept_language(&HeaderMap::new()) == Language::En);
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
        assert!(Language::from_accept_language(&headers) == Language::En);
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
            assert!(Language::from_code(language.code()) == Some(language));
        }
        assert!(Language::from_code("FI") == Some(Language::Fi));
        assert!(Language::from_code("sv").is_none());
        assert!(Language::from_code("").is_none());
        assert!(Language::from_code("*").is_none());
    }

    #[test]
    fn query_parameter_overrides_accept_language() {
        let finnish = accept_language("fi");
        let english = accept_language("en");
        assert!(Language::from_request(&query(Some("en")), &finnish) == Language::En);
        assert!(Language::from_request(&query(Some("fi")), &english) == Language::Fi);
        assert!(Language::from_request(&query(Some("FI")), &english) == Language::Fi);
        assert!(Language::from_request(&query(None), &finnish) == Language::Fi);
    }

    #[test]
    fn unknown_query_language_falls_back_to_english_not_header() {
        let finnish = accept_language("fi");
        assert!(Language::from_request(&query(Some("sv")), &finnish) == Language::En);
        assert!(Language::from_request(&query(Some("")), &finnish) == Language::En);
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
        get_from(app(Path::new("dist"), false), uri, accept_language).await
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
        assert!(body.contains(r#" width="184" height="184" alt="Profile picture">"#));
        assert!(
            body.contains("</blockquote><figcaption>- Master Tung Chen Wei</figcaption></figure>")
        );
        assert!(body.contains(r#"<html lang="en">"#));
        assert!(body.contains("<title>Online Tai Chi Classes ☯ Taiji-Timo</title>"));
        assert!(
            !body.contains("role='banner'"),
            "<header> is already the banner landmark"
        );
        assert!(body.contains("<svg viewBox='0 0 360 360' aria-hidden='true'"));
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
        for name in ["styles.css", "img/profile.jpg"] {
            let url = assets::url(name);
            assert!(url.contains("?v="), "{url}");
            assert!(body.contains(&format!(r#"="{url}""#)), "{url} not linked");
        }
        Ok(())
    }

    #[tokio::test]
    async fn versioned_static_files_are_cached_immutably() -> TestResult {
        for name in ["styles.css", "img/profile.jpg"] {
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
            app(Path::new("dist"), true),
            &assets::url("styles.css"),
            None,
        )
        .await?;
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
        Ok(())
    }
}
