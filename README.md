# Taiji-Timo

*Online Tai Chi classes, in English and Finnish — https://www.taichichuan.mobi*

A single-page site served by a small Rust server (axum + maud). The language
comes from `?lang=en` / `?lang=fi`, or else from the browser's languages.

## Development

Enter the dev shell with direnv (`.envrc`) or `nix develop`, then:

```sh
just watch     # run on http://localhost:3000, restart and reload on changes
cargo test     # tests
nix build      # the deployable package, into ./result
```

The dev shell also enables a pre-commit hook (`.githooks/pre-commit`) that runs
`cargo fmt --check`, `cargo clippy` and `cargo test`. Skip it once with
`git commit --no-verify`.

## Configuration

All settings are optional environment variables, read also from a `.env` file.
See `.env.example` for the list and defaults: `HOST`, `PORT`, `SITE_URL`,
`STATIC_DIR`, `DEV` and `RUST_LOG`.

The server refuses to start if `STATIC_DIR` is missing the files the page links
to, and logs which ones.

## Deployment

`nix/module.nix` is a NixOS module that runs the site as a hardened systemd
service, one per instance, listening on 127.0.0.1:3100 by default. Put a reverse
proxy with TLS, such as nginx, in front. Usage is described at the top of the
module.

## Content

- Texts in both languages: `Translations::for_language` in `src/main.rs`.
  Links inside texts are written as `{name}` and filled in by the handlers.
- Styles: `dist/styles.css`.
- Logo and favicon: `dist/favicon.svg`, also embedded as the page logo.
  `favicon.ico` and `apple-touch-icon.png` are made from it.
- Fonts are self-hosted in `dist/fonts` (SIL Open Font License). The Latin files
  in `PRELOADED_FONTS` must match the `@font-face` URLs in `styles.css`.

Files listed in `VERSIONED_FILES` (`src/assets.rs`) get a content hash in their
URL and are cached for a year, so add new linked files there.

### Profile picture

`dist/img/profile.jpg` (288 px, for 1× and 2× screens) and `profile-432.jpg`
(3× screens) are the same crop of a 2560×1600 screenshot from the class video,
which is kept outside the repository. To make them again from it:

```sh
magick screenshot.png -crop 654x654+599+477 +repage -filter Lanczos -resize 288x288 \
    -strip -sampling-factor 4:4:4 -interlace JPEG -quality 90 dist/img/profile.jpg
magick screenshot.png -crop 654x654+599+477 +repage -filter Lanczos -resize 432x432 \
    -strip -sampling-factor 4:4:4 -interlace JPEG -quality 85 dist/img/profile-432.jpg
```

Update `width`/`height` in `src/main.rs` if the size changes.
