# Read .env too, so that `watch` also restarts on lesson changes in CONTENT_DIR.
set dotenv-load

# Restart on code, static file, lesson and .env changes.
watch:
    DEV= watchexec --restart --watch src --watch dist --watch Cargo.toml \
        {{ if path_exists(".env") == "true" { "--watch .env" } else { "" } }} \
        {{ if env("CONTENT_DIR", "") != "" { "--watch " + env("CONTENT_DIR") } else { "" } }} -- cargo run

release:
    cargo build --release
