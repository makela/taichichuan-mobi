# Restart on code, static file and .env changes.
watch:
    DEV= watchexec --restart --watch src --watch dist --watch Cargo.toml \
        {{ if path_exists(".env") == "true" { "--watch .env" } else { "" } }} -- cargo run

release:
    cargo build --release
