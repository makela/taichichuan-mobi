# Taiji-Timo

*Online Tai Chi classes, in English and Finnish — https://www.taichichuan.mobi*

A single-page site served by a small Rust server (axum + maud). The language
comes from `?lang=en` / `?lang=fi`, or else from the browser's languages.

## Development

Enter the dev shell with direnv (`.envrc`) or `nix develop`, then:

```sh
just watch       # run on http://localhost:3000, restart and reload on changes
cargo test       # tests
nix build        # the deployable package, into ./result
nix flake check  # also runs the NixOS module in a virtual machine (nix/test.nix)
```

The dev shell also enables a pre-commit hook (`.githooks/pre-commit`) that runs
a check that staged images carry no GPS location, `cargo fmt --check`,
`cargo clippy` and `cargo test`. Skip it once with
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

### Lessons

The course is members' material and is not in this repository. It is read at
start-up from the directory in `CONTENT_DIR`: `course.toml` lists its sets,
which are forms (the slow set, the fast set, weapons) or practices beside them,
and each set's `<set>/lessons.toml` lists its lessons, with each lesson's text in
`<set>/<lesson>/en.md` and `fi.md` (Markdown; subheadings with `###`). The format
is described at the top of `src/lessons.rs`, and `tests/lessons` has a made-up
example. The server refuses to start if the course has mistakes, such as a
movement in two lessons, and says what is wrong.

A lesson can have a video: `<set>/<lesson>/video.mp4`, or `video-en.mp4` and
`video-fi.mp4` for each language. The site serves it only to those who may open
the lesson, and a note under it says that downloading or sharing the
videos is against the terms of use. Before adding a video, remove its metadata,
which in phone videos includes the location, and move its index to the start so
that it starts playing before it has all loaded:

```sh
ffmpeg -i original.mp4 -map_metadata -1 -c copy -movflags +faststart video.mp4
```

### Members

With `CONTENT_DIR` set, visitors can register and log in at `/register` and
`/login` with a link sent by email; there are no passwords. Accounts, sessions
and the lessons each member has passed are in an SQLite database
(`DATABASE_URL`, tables in `migrations/`). Free lessons and practices are open
to everyone logged in. Members take the lessons of each form in order, each
opening when the one before it is passed, and passed lessons stay open. A form
that requires another opens when that one is finished. Practices are open to
members at once, or once a given lesson is passed. `/account` has logging out
and deleting the account.

For now all this runs in development mode only, as there are no payments yet.
Without `SMTP_HOST` the emails aren't sent and the link is logged instead, and
the account page has a button to start or end a membership for trying out.
Lessons passed by a quiz or a video review can be marked practised there too
until those are made.

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

## License

The code is under the MIT license, see `LICENSE`. The site's texts, in
`Translations` in `src/main.rs`, and the photos in `dist/img/` are all rights
reserved. The fonts in `dist/fonts/` are under the SIL Open Font License, with
their license files next to them.
