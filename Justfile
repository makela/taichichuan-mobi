[parallel]
watch: dev tailwind

dev:
    DEV= cargo watch --no-gitignore -x "run"

tailwind:
    tailwindcss -i ./styles.css -o ./dist/styles.css --watch

release:
    tailwindcss -i ./styles.css -o ./dist/styles.css --minify
    cargo build --release
