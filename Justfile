# Restart on code and static file changes.
watch:
    DEV= watchexec --restart --watch src --watch dist --watch Cargo.toml -- cargo run

release:
    cargo build --release
