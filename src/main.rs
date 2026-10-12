use axum::{routing::get, Router};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace,
    trace::TraceLayer,
};
use tower_livereload::LiveReloadLayer;
use tracing::Level;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("www=debug,tower_http=info"))
                .unwrap(),
        )
        .init();

    let app = Router::new()
        .nest_service("/static", ServeDir::new("dist"))
        .route("/", get(index))
        .route_service("/favicon.ico", ServeFile::new("dist/favicon.ico"))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
        );
    let app = if std::env::var("DEV").is_ok() {
        app.layer(LiveReloadLayer::new())
    } else {
        app
    };
    let port = std::env::var("PORT").unwrap_or("3000".to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

async fn index() -> Markup {
    html! {
        (DOCTYPE)
        head {
            title { "Timo Mäkelä - Software Entrepreneur" }
            meta charset="utf-8";
            meta http-equiv="X-UA-Compatible" content="IE=edge";
            meta name="viewport" content="width=device-width, initial-scale=1";
            link rel="preconnect" href="https://fonts.googleapis.com";
            link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="true";
            link href="https://fonts.googleapis.com/css2?family=Nunito&family=Gloria+Hallelujah&display=swap" rel="stylesheet";
            link href="/static/styles.css" rel="stylesheet";
        }
        body {
            header {
                //p { "In English or suomeksi" }
                (PreEscaped("
                <div class='logo' role='banner'><svg viewBox='0 0 360 360'><circle r='180' transform='translate(180, 180)'></circle><circle fill='white' r='180' transform='translate(180, 180)'></circle><path transform='translate(180, 180) rotate(90)' d='M-180,0A180,180,0,1,1,180,0L0,0Z'></path><circle r='90' transform='translate(180, 270)'></circle><circle fill='white' r='90' transform='translate(180, 90)'></circle><circle fill='white' r='27' transform='translate(180, 270)'></circle><circle r='27' transform='translate(180, 90)'></circle><path stroke='black' stroke-width='2' fill='none' d='M276.714112750397,330.62330628061346A179,179,0,1,1,359,179.99999999999994'></path></svg></div>
                "))
                h1 { "Tai Chi Teaching Online" } // Chuan
                blockquote { "When the student is ready, the teacher will appear" }
            }
            main {
                h2 { "Who am I?" }
                p {
                    "My name is Timo and "
                    a href="https://en.wikipedia.org/wiki/Tai_chi" { "Tai Chi" }
                    " is my passion. My teacher is "
                    a href="https://www.facebook.com/taichichuanboscofalconeria/" { "Rami Salo" }
                    " from "
                    a href="https://taichichuan.fi" { "Tai Chi Chuan ry" }
                    " and my master is "
                    a href="http://tungtaichi.com" { "Tung Chen Wei" }
                    ". I also write "
                    a href="https://www.taichiwayfarer.com" { "a blog" }
                    " from my own personal learning experiences."
                }
                h2 { "What do I offer?" }
                p {
                    "My nomadic lifestyle does not support having a weekly class in one place,
                    so I offer private online classes to new and old students of the art. Send me "
                    a rel="noreferrer" href="mailto:taichiwayfarer@gmail.com?subject=Tai%20Chi%20" {
                        "an email"
                    }
                    " to schedule a free 15 minutes introductory class!"
                }
                h2 { "What would you need?" }
                p {
                    "You will need to be able to make a video call (a smart phone or a laptop)
                    and some room to practice a few movements at a time."
                }
            }
            footer {
                p { "Taiji-Timo © 2212553-3" }
            }
        }
    }
}
