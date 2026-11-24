//! Lessons of the form, read from a content directory kept outside this
//! repository (`CONTENT_DIR`):
//!
//! ```text
//! lessons.toml           the lessons in the order they are taken
//! <slug>/en.md           lesson text in English, Markdown
//! <slug>/fi.md           lesson text in Finnish
//! <slug>/video.mp4       optional video, the same in both languages, or
//! <slug>/video-en.mp4    a video in English
//! <slug>/video-fi.mp4    and in Finnish
//! ```
//!
//! A video in one language only is shown in both.
//!
//! Each lesson in `lessons.toml`:
//!
//! ```toml
//! [[lesson]]
//! slug = "preparation"       # in the URL: a-z, 0-9 and -
//! title = { en = "Preparation", fi = "Valmistautuminen" }
//! movements = [1]            # numbers 1-89 of the form, repetitions included
//! passing = "practice"       # "practice", "quiz" or "review"
//! free = true                # optional; free lessons come first
//! ```

use pulldown_cmark::{Event, Options, Parser};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::Language;

/// Movements in the form, numbered from 1.
pub const MOVEMENT_COUNT: u8 = 89;

/// How a member passes a lesson, which opens the next one.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Passing {
    /// The member marks the lesson practised.
    Practice,
    /// The member answers a quiz.
    Quiz,
    /// The member sends a video, and the teacher approves it.
    Review,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(rename = "lesson", default)]
    lessons: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    slug: String,
    title: Titles,
    movements: Vec<u8>,
    passing: Passing,
    #[serde(default)]
    free: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Titles {
    en: String,
    fi: String,
}

pub struct Lesson {
    pub slug: String,
    pub movements: Vec<u8>,
    pub passing: Passing,
    pub free: bool,
    titles: Titles,
    /// Rendered from Markdown, in English and in Finnish.
    html: [String; 2],
    /// The video files for English and Finnish, if any.
    videos: Option<[PathBuf; 2]>,
}

impl Lesson {
    pub fn title(&self, language: Language) -> &str {
        match language {
            Language::En => &self.titles.en,
            Language::Fi => &self.titles.fi,
        }
    }

    /// The lesson text as HTML, safe to include in a page as is.
    pub fn html(&self, language: Language) -> &str {
        match language {
            Language::En => &self.html[0],
            Language::Fi => &self.html[1],
        }
    }

    /// The lesson's video file in `language`, if it has a video.
    pub fn video(&self, language: Language) -> Option<&Path> {
        let [en, fi] = self.videos.as_ref()?;
        Some(match language {
            Language::En => en,
            Language::Fi => fi,
        })
    }
}

/// The videos of the lesson in `lesson_dir`, for English and Finnish.
fn find_videos(lesson_dir: &Path) -> Option<[PathBuf; 2]> {
    let existing = |name: &str| {
        let path = lesson_dir.join(name);
        path.is_file().then_some(path)
    };
    let common = existing("video.mp4");
    let en = existing("video-en.mp4").or_else(|| common.clone());
    let fi = existing("video-fi.mp4").or(common);
    match (en, fi) {
        (Some(en), Some(fi)) => Some([en, fi]),
        (Some(only), None) | (None, Some(only)) => Some([only.clone(), only]),
        (None, None) => None,
    }
}

pub struct Lessons(Vec<Lesson>);

impl Lessons {
    /// Reads and checks every lesson in `dir`, so mistakes in the content
    /// show up when the server starts rather than when a page is opened.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let manifest_path = dir.join("lessons.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&manifest)
            .map_err(|error| format!("{}: {error}", manifest_path.display()))?;

        let mut slugs = HashSet::new();
        let mut movements = HashSet::new();
        let mut paid_seen = false;
        let mut lessons = Vec::with_capacity(manifest.lessons.len());
        for entry in manifest.lessons {
            let slug = &entry.slug;
            if slug.is_empty()
                || !slug
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(format!(
                    "lesson {slug:?}: a slug may only have a-z, 0-9 and -"
                ));
            }
            if !slugs.insert(slug.clone()) {
                return Err(format!("lesson {slug:?} is listed twice"));
            }
            if entry.titles_empty() {
                return Err(format!("lesson {slug:?}: a title is empty"));
            }
            if entry.movements.is_empty() {
                return Err(format!("lesson {slug:?}: no movements"));
            }
            for &movement in &entry.movements {
                if !(1..=MOVEMENT_COUNT).contains(&movement) {
                    return Err(format!(
                        "lesson {slug:?}: movement {movement} is not between 1 and {MOVEMENT_COUNT}"
                    ));
                }
                if !movements.insert(movement) {
                    return Err(format!(
                        "lesson {slug:?}: movement {movement} is already in another lesson"
                    ));
                }
            }
            if entry.free && paid_seen {
                return Err(format!(
                    "lesson {slug:?} is free but comes after a paid lesson; free lessons come first"
                ));
            }
            paid_seen |= !entry.free;

            let text = |code: &str| {
                let path = dir.join(slug).join(format!("{code}.md"));
                std::fs::read_to_string(&path)
                    .map(|text| render_markdown(&text))
                    .map_err(|error| format!("{}: {error}", path.display()))
            };
            let html = [text("en")?, text("fi")?];
            let videos = find_videos(&dir.join(slug));
            lessons.push(Lesson {
                slug: entry.slug,
                movements: entry.movements,
                passing: entry.passing,
                free: entry.free,
                titles: entry.title,
                html,
                videos,
            });
        }
        Ok(Self(lessons))
    }

    pub fn all(&self) -> &[Lesson] {
        &self.0
    }

    /// The lesson and its position, counted from 0.
    pub fn find(&self, slug: &str) -> Option<(usize, &Lesson)> {
        self.0
            .iter()
            .enumerate()
            .find(|(_, lesson)| lesson.slug == slug)
    }

    /// Whether `learner` may open the lesson at `position`. Free lessons are
    /// open to everyone logged in. Members take the others in order: a
    /// lesson opens when the one before it is passed, and passed lessons
    /// stay open.
    #[must_use]
    pub fn access(&self, position: usize, learner: Option<&Learner>) -> Access {
        let Some(lesson) = self.0.get(position) else {
            return Access::NeedsPrevious;
        };
        let Some(learner) = learner else {
            return Access::NeedsLogin;
        };
        let done = learner.completed.contains(&lesson.slug);
        if lesson.free {
            return if done { Access::Done } else { Access::Open };
        }
        if !learner.member {
            return Access::NeedsMembership;
        }
        let previous_done = position
            .checked_sub(1)
            .and_then(|previous| self.0.get(previous))
            .is_none_or(|previous| learner.completed.contains(&previous.slug));
        match (done, previous_done) {
            (true, _) => Access::Done,
            (false, true) => Access::Open,
            (false, false) => Access::NeedsPrevious,
        }
    }
}

/// Who is opening a lesson, as far as access to it goes.
pub struct Learner<'a> {
    pub member: bool,
    /// Slugs of the lessons passed.
    pub completed: &'a HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Access {
    Open,
    /// Open, and passed already.
    Done,
    NeedsLogin,
    NeedsMembership,
    /// The lesson before it isn't passed yet.
    NeedsPrevious,
}

impl Access {
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Done)
    }
}

impl Entry {
    fn titles_empty(&self) -> bool {
        self.title.en.trim().is_empty() || self.title.fi.trim().is_empty()
    }
}

/// Markdown to HTML. HTML written in the Markdown is shown as text, so the
/// lesson texts can't add scripts, styles or anything else the pages'
/// Content-Security-Policy would block.
fn render_markdown(source: &str) -> String {
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    )
    .map(|event| match event {
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        other => other,
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A content directory with `manifest` and an English and Finnish text
    /// for each of `slugs`, removed when dropped.
    struct TempContent(std::path::PathBuf);

    impl TempContent {
        fn new(name: &str, manifest: &str, slugs: &[&str]) -> Result<Self, std::io::Error> {
            let dir = std::env::temp_dir().join(format!(
                "taichichuan-mobi-lessons-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("lessons.toml"), manifest)?;
            for slug in slugs {
                std::fs::create_dir_all(dir.join(slug))?;
                std::fs::write(dir.join(slug).join("en.md"), "Text")?;
                std::fs::write(dir.join(slug).join("fi.md"), "Teksti")?;
            }
            Ok(Self(dir))
        }
    }

    impl Drop for TempContent {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lesson(slug: &str, movements: &str, extra: &str) -> String {
        format!(
            "[[lesson]]\nslug = \"{slug}\"\ntitle = {{ en = \"E\", fi = \"F\" }}\n\
             movements = {movements}\npassing = \"practice\"\n{extra}\n"
        )
    }

    fn load_error(
        name: &str,
        manifest: &str,
        slugs: &[&str],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let content = TempContent::new(name, manifest, slugs)?;
        match Lessons::load(&content.0) {
            Ok(_) => Err(format!("{name}: loaded").into()),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn loads_test_content() -> TestResult {
        let lessons = Lessons::load(Path::new("tests/lessons"))?;
        let slugs: Vec<&str> = lessons
            .all()
            .iter()
            .map(|lesson| lesson.slug.as_str())
            .collect();
        assert_eq!(slugs, ["preparation", "opening", "single-whip"]);
        let (position, whip) = lessons.find("single-whip").ok_or("single-whip")?;
        assert_eq!(position, 2);
        assert_eq!(whip.movements, [5, 30]);
        assert_eq!(whip.passing, Passing::Review);
        assert!(!whip.free);
        assert_eq!(whip.title(Language::Fi), "Yksittäinen ruoska");
        assert!(lessons.find("missing").is_none());

        let (_, preparation) = lessons.find("preparation").ok_or("preparation")?;
        assert_eq!(preparation.video(Language::En), None);
        let (_, opening) = lessons.find("opening").ok_or("opening")?;
        let video = Path::new("tests/lessons/opening/video.mp4");
        assert_eq!(opening.video(Language::En), Some(video));
        assert_eq!(opening.video(Language::Fi), Some(video));
        // single-whip has a Finnish video only, shown in English too.
        let video = Path::new("tests/lessons/single-whip/video-fi.mp4");
        assert_eq!(whip.video(Language::En), Some(video));
        assert_eq!(whip.video(Language::Fi), Some(video));
        Ok(())
    }

    #[test]
    fn free_lessons_are_open_and_the_rest_go_in_order_for_members() -> TestResult {
        use Access::{Done, NeedsMembership, NeedsPrevious, Open};
        let lessons = Lessons::load(Path::new("tests/lessons"))?;
        let access = |completed: &[&str], member: bool| {
            let completed: HashSet<String> =
                completed.iter().map(|slug| (*slug).to_string()).collect();
            let learner = Learner {
                member,
                completed: &completed,
            };
            (0..4)
                .map(|position| lessons.access(position, Some(&learner)))
                .collect::<Vec<_>>()
        };
        // preparation and opening are free, single-whip isn't; there is no
        // fourth lesson.
        assert_eq!(
            access(&[], false),
            [Open, Open, NeedsMembership, NeedsPrevious]
        );
        assert_eq!(
            access(&["opening"], false),
            [Open, Done, NeedsMembership, NeedsPrevious]
        );
        assert_eq!(
            access(&[], true),
            [Open, Open, NeedsPrevious, NeedsPrevious]
        );
        assert_eq!(
            access(&["opening"], true),
            [Open, Done, Open, NeedsPrevious]
        );
        // Passed lessons stay open even if an earlier one isn't passed.
        assert_eq!(
            access(&["single-whip"], true),
            [Open, Open, Done, NeedsPrevious]
        );
        assert_eq!(lessons.access(0, None), Access::NeedsLogin);
        assert!(Open.is_open() && Done.is_open() && !NeedsPrevious.is_open());
        Ok(())
    }

    #[test]
    fn markdown_is_rendered_and_html_in_it_escaped() {
        assert_eq!(
            render_markdown("### Feet\n\n*Slowly* <script>x</script>\n\n<div>y</div>"),
            "<h3>Feet</h3>\n<p><em>Slowly</em> &lt;script&gt;x&lt;/script&gt;</p>\n&lt;div&gt;y&lt;/div&gt;"
        );
    }

    #[test]
    fn rejects_mistakes_in_the_content() -> TestResult {
        let cases: &[(&str, String, &[&str], &str)] = &[
            (
                "slug",
                lesson("Bad Slug", "[1]", ""),
                &["Bad Slug"],
                "a slug may only",
            ),
            (
                "twice",
                lesson("a", "[1]", "") + &lesson("a", "[2]", ""),
                &["a"],
                "listed twice",
            ),
            (
                "range",
                lesson("a", "[90]", ""),
                &["a"],
                "not between 1 and 89",
            ),
            (
                "zero",
                lesson("a", "[0]", ""),
                &["a"],
                "not between 1 and 89",
            ),
            ("none", lesson("a", "[]", ""), &["a"], "no movements"),
            (
                "shared",
                lesson("a", "[1, 2]", "") + &lesson("b", "[2]", ""),
                &["a", "b"],
                "movement 2 is already",
            ),
            (
                "free-later",
                lesson("a", "[1]", "") + &lesson("b", "[2]", "free = true"),
                &["a", "b"],
                "free lessons come first",
            ),
            ("missing-text", lesson("a", "[1]", ""), &[], "en.md"),
            (
                "unknown-field",
                lesson("a", "[1]", "vidoe = \"x\""),
                &["a"],
                "vidoe",
            ),
            (
                "passing",
                lesson("a", "[1]", "").replace("practice", "exam"),
                &["a"],
                "exam",
            ),
            (
                "empty-title",
                lesson("a", "[1]", "").replace("en = \"E\"", "en = \" \""),
                &["a"],
                "a title is empty",
            ),
        ];
        for (name, manifest, slugs, expected) in cases {
            let error = load_error(name, manifest, slugs)?;
            assert!(error.contains(expected), "{name}: {error}");
        }
        Ok(())
    }

    #[test]
    fn missing_manifest_names_the_file() -> TestResult {
        let error = Lessons::load(Path::new("tests/no-such-dir"))
            .err()
            .ok_or("loaded")?;
        assert!(error.contains("lessons.toml"), "{error}");
        Ok(())
    }
}
