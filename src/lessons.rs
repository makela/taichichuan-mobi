//! The course: sets of lessons, such as the slow set, the fast set or a
//! weapon form, and collections of practices beside them. Read from a content
//! directory kept outside this repository (`CONTENT_DIR`):
//!
//! ```text
//! course.toml                   the sets, in the order they are shown
//! <set>/lessons.toml            the set's lessons, in order
//! <set>/<lesson>/en.md          lesson text in English, Markdown
//! <set>/<lesson>/fi.md          lesson text in Finnish
//! <set>/<lesson>/video.mp4      optional video, the same in both languages, or
//! <set>/<lesson>/video-en.mp4   a video in English
//! <set>/<lesson>/video-fi.mp4   and in Finnish
//! ```
//!
//! A video in one language only is shown in both.
//!
//! Each set in `course.toml`:
//!
//! ```toml
//! [[set]]
//! slug = "slow-set"            # in the URL: a-z, 0-9 and -
//! title = { en = "Slow Set", fi = "Hidas sarja" }
//! movements = 89               # how many movements the form has
//! requires = ["other-set"]     # optional: sets to finish first, listed above
//!
//! [[set]]
//! slug = "practices"
//! title = { en = "Practices", fi = "Harjoitteet" }
//! practices = true             # practices instead of a form
//! ```
//!
//! The lessons of a form are taken in order, each passed to open the next.
//! In `<set>/lessons.toml`:
//!
//! ```toml
//! [[lesson]]
//! slug = "preparation"
//! title = { en = "Preparation", fi = "Valmistautuminen" }
//! movements = [1]              # the form's movements, repetitions included
//! passing = "practice"         # "practice", "quiz" or "review"
//! free = true                  # optional; free lessons come first
//! ```
//!
//! Practices are not passed, and need no order:
//!
//! ```toml
//! [[lesson]]
//! slug = "standing"
//! title = { en = "Standing", fi = "Seisominen" }
//! free = true                          # optional: open to all logged in
//! opens_after = "slow-set/opening"     # optional: once that lesson is passed
//! ```
//!
//! Without either, a practice is open to members from the start. Free lessons
//! and practices are only in sets that require no other set.

use pulldown_cmark::{Event, Options, Parser};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::Language;

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
struct CourseFile {
    #[serde(rename = "set", default)]
    sets: Vec<SetEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEntry {
    slug: String,
    title: Titles,
    #[serde(default)]
    practices: bool,
    movements: Option<u8>,
    #[serde(default)]
    requires: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LessonsFile {
    #[serde(rename = "lesson", default)]
    lessons: Vec<LessonEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LessonEntry {
    slug: String,
    title: Titles,
    #[serde(default)]
    movements: Vec<u8>,
    passing: Option<Passing>,
    #[serde(default)]
    free: bool,
    opens_after: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Titles {
    en: String,
    fi: String,
}

impl Titles {
    fn get(&self, language: Language) -> &str {
        match language {
            Language::En => &self.en,
            Language::Fi => &self.fi,
        }
    }

    fn any_empty(&self) -> bool {
        self.en.trim().is_empty() || self.fi.trim().is_empty()
    }
}

/// A lesson by its set's and its own position, counted from 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LessonId {
    pub set: usize,
    pub lesson: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A form, learned lesson by lesson in order.
    Form,
    /// Practices, open by their own rules.
    Practices,
}

pub struct Set {
    pub slug: String,
    pub kind: Kind,
    titles: Titles,
    /// Positions of the sets to finish first.
    requires: Vec<usize>,
    pub lessons: Vec<Lesson>,
}

impl Set {
    pub fn title(&self, language: Language) -> &str {
        self.titles.get(language)
    }
}

pub struct Lesson {
    pub slug: String,
    /// "set/lesson", as passed lessons are stored.
    pub key: String,
    pub movements: Vec<u8>,
    /// How a lesson of a form is passed; practices aren't.
    pub passing: Option<Passing>,
    pub free: bool,
    /// For a practice: the lesson to pass before it opens.
    opens_after: Option<LessonId>,
    titles: Titles,
    /// Rendered from Markdown, in English and in Finnish.
    html: [String; 2],
    /// The video files for English and Finnish, if any.
    videos: Option<[PathBuf; 2]>,
}

impl Lesson {
    pub fn title(&self, language: Language) -> &str {
        self.titles.get(language)
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

fn check_slug(slug: &str, what: &str) -> Result<(), String> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "{what} {slug:?}: a slug may only have a-z, 0-9 and -"
        ));
    }
    Ok(())
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Checks a lesson of a set with `form_movements` movements, or of
/// practices with None. `movements` and `paid_seen` carry what the set's
/// lessons before it have: their movements, and whether any isn't free.
fn check_lesson(
    key: &str,
    lesson: &LessonEntry,
    form_movements: Option<u8>,
    movements: &mut HashSet<u8>,
    paid_seen: &mut bool,
) -> Result<(), String> {
    let Some(count) = form_movements else {
        if !lesson.movements.is_empty() || lesson.passing.is_some() {
            return Err(format!(
                "lesson {key:?}: practices have no movements or passing"
            ));
        }
        if lesson.free && lesson.opens_after.is_some() {
            return Err(format!(
                "lesson {key:?}: a free practice can't wait for opens_after"
            ));
        }
        return Ok(());
    };
    if lesson.movements.is_empty() {
        return Err(format!("lesson {key:?}: no movements"));
    }
    for &movement in &lesson.movements {
        if !(1..=count).contains(&movement) {
            return Err(format!(
                "lesson {key:?}: movement {movement} is not between 1 and {count}"
            ));
        }
        if !movements.insert(movement) {
            return Err(format!(
                "lesson {key:?}: movement {movement} is already in another lesson"
            ));
        }
    }
    if lesson.passing.is_none() {
        return Err(format!(
            "lesson {key:?}: say how it is passed, e.g. passing = \"practice\""
        ));
    }
    if lesson.opens_after.is_some() {
        return Err(format!(
            "lesson {key:?}: lessons of a form open in order, without opens_after"
        ));
    }
    if lesson.free && *paid_seen {
        return Err(format!(
            "lesson {key:?} is free but comes after a paid lesson; free lessons come first"
        ));
    }
    *paid_seen |= !lesson.free;
    Ok(())
}

/// The kind of the set in `entry`.
fn set_kind(entry: &SetEntry) -> Result<Kind, String> {
    let set = &entry.slug;
    match (entry.practices, entry.movements) {
        (false, Some(count)) if count > 0 => Ok(Kind::Form),
        (false, _) => Err(format!(
            "set {set:?}: give the number of movements, or practices = true"
        )),
        (true, None) => Ok(Kind::Practices),
        (true, Some(_)) => Err(format!("set {set:?}: practices have no movements")),
    }
}

/// Positions of the sets that the set in `entry` requires, among `sets`
/// listed above it.
fn required_sets(entry: &SetEntry, sets: &[Set]) -> Result<Vec<usize>, String> {
    let set = &entry.slug;
    entry
        .requires
        .iter()
        .map(
            |required| match sets.iter().position(|other| other.slug == *required) {
                Some(position) if sets[position].kind == Kind::Form => Ok(position),
                Some(_) => Err(format!(
                    "set {set:?} requires {required:?}, which is practices and can't be finished"
                )),
                None => Err(format!(
                    "set {set:?} requires {required:?}, which isn't listed above it"
                )),
            },
        )
        .collect()
}

pub struct Course {
    sets: Vec<Set>,
}

impl Course {
    /// Reads and checks the whole course in `dir`, so mistakes in the content
    /// show up when the server starts rather than when a page is opened.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let course: CourseFile = read_toml(&dir.join("course.toml"))?;
        let mut sets: Vec<Set> = Vec::with_capacity(course.sets.len());
        // Each practice's opens_after, resolved once every set is read.
        let mut opens_after = Vec::new();
        for entry in course.sets {
            let set = &entry.slug;
            check_slug(set, "set")?;
            if sets.iter().any(|other| other.slug == *set) {
                return Err(format!("set {set:?} is listed twice"));
            }
            if entry.title.any_empty() {
                return Err(format!("set {set:?}: a title is empty"));
            }
            let kind = set_kind(&entry)?;
            let requires = required_sets(&entry, &sets)?;

            let lessons_file: LessonsFile = read_toml(&dir.join(set).join("lessons.toml"))?;
            if lessons_file.lessons.is_empty() {
                return Err(format!("set {set:?} has no lessons"));
            }
            let mut lessons: Vec<Lesson> = Vec::with_capacity(lessons_file.lessons.len());
            let mut movements = HashSet::new();
            let mut paid_seen = false;
            for lesson in lessons_file.lessons {
                let key = format!("{set}/{}", lesson.slug);
                check_slug(&lesson.slug, "lesson")?;
                if lessons.iter().any(|other| other.slug == lesson.slug) {
                    return Err(format!("lesson {key:?} is listed twice"));
                }
                if lesson.title.any_empty() {
                    return Err(format!("lesson {key:?}: a title is empty"));
                }
                if lesson.free && !requires.is_empty() {
                    return Err(format!(
                        "lesson {key:?} is free, but its set requires another set"
                    ));
                }
                check_lesson(
                    &key,
                    &lesson,
                    entry.movements,
                    &mut movements,
                    &mut paid_seen,
                )?;
                if let Some(after) = lesson.opens_after {
                    opens_after.push((
                        LessonId {
                            set: sets.len(),
                            lesson: lessons.len(),
                        },
                        after,
                    ));
                }

                let lesson_dir = dir.join(set).join(&lesson.slug);
                let text = |code: &str| {
                    let path = lesson_dir.join(format!("{code}.md"));
                    std::fs::read_to_string(&path)
                        .map(|text| render_markdown(&text))
                        .map_err(|error| format!("{}: {error}", path.display()))
                };
                let html = [text("en")?, text("fi")?];
                lessons.push(Lesson {
                    slug: lesson.slug,
                    key,
                    movements: lesson.movements,
                    passing: lesson.passing,
                    free: lesson.free,
                    opens_after: None,
                    titles: lesson.title,
                    html,
                    videos: find_videos(&lesson_dir),
                });
            }
            sets.push(Set {
                slug: entry.slug,
                kind,
                titles: entry.title,
                requires,
                lessons,
            });
        }

        let mut course = Self { sets };
        course.resolve_opens_after(opens_after)?;
        Ok(course)
    }

    /// Sets each practice's `opens_after` from its "set/lesson", checking
    /// that it names a lesson of a form.
    fn resolve_opens_after(&mut self, opens_after: Vec<(LessonId, String)>) -> Result<(), String> {
        for (id, after) in opens_after {
            let found = after
                .split_once('/')
                .and_then(|(set, lesson)| self.find(set, lesson))
                .map(|(after_id, _)| after_id);
            let practice = self.sets[id.set].lessons[id.lesson].key.clone();
            match found {
                Some(after_id) if self.sets[after_id.set].kind == Kind::Form => {
                    self.sets[id.set].lessons[id.lesson].opens_after = Some(after_id);
                }
                Some(_) => {
                    return Err(format!(
                        "practice {practice:?} opens after {after:?}, which is a practice and is never passed"
                    ));
                }
                None => {
                    return Err(format!(
                        "practice {practice:?} opens after {after:?}, which isn't a lesson (set/lesson)"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn sets(&self) -> &[Set] {
        &self.sets
    }

    pub fn find_set(&self, slug: &str) -> Option<(usize, &Set)> {
        self.sets
            .iter()
            .enumerate()
            .find(|(_, set)| set.slug == slug)
    }

    pub fn find(&self, set: &str, lesson: &str) -> Option<(LessonId, &Lesson)> {
        let (set_position, found) = self.find_set(set)?;
        found
            .lessons
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.slug == lesson)
            .map(|(position, lesson)| {
                (
                    LessonId {
                        set: set_position,
                        lesson: position,
                    },
                    lesson,
                )
            })
    }

    pub fn lesson(&self, id: LessonId) -> Option<&Lesson> {
        self.sets.get(id.set)?.lessons.get(id.lesson)
    }

    /// How many of the set's lessons are passed.
    #[must_use]
    pub fn passed(&self, set: usize, completed: &HashSet<String>) -> usize {
        self.sets.get(set).map_or(0, |set| {
            set.lessons
                .iter()
                .filter(|lesson| completed.contains(&lesson.key))
                .count()
        })
    }

    fn finished(&self, set: usize, completed: &HashSet<String>) -> bool {
        self.sets
            .get(set)
            .is_some_and(|found| self.passed(set, completed) == found.lessons.len())
    }

    /// The first set that `set` requires and that isn't finished, if any.
    #[must_use]
    pub fn missing_set(&self, set: usize, completed: &HashSet<String>) -> Option<usize> {
        self.sets
            .get(set)?
            .requires
            .iter()
            .copied()
            .find(|&required| !self.finished(required, completed))
    }

    /// Whether `learner` may open the lesson `id`. Free lessons and practices
    /// are open to everyone logged in. Members take the lessons of a form in
    /// order once the sets it requires are finished: a lesson opens when the
    /// one before it is passed, and passed lessons stay open. Practices open
    /// to members at once, or when the lesson they wait for is passed.
    #[must_use]
    pub fn access(&self, id: LessonId, learner: Option<&Learner>) -> Access {
        let (Some(set), Some(lesson)) = (self.sets.get(id.set), self.lesson(id)) else {
            return Access::NeedsPrevious;
        };
        let Some(learner) = learner else {
            return Access::NeedsLogin;
        };
        let done = learner.completed.contains(&lesson.key);
        if lesson.free {
            return if done { Access::Done } else { Access::Open };
        }
        if !learner.member {
            return Access::NeedsMembership;
        }
        if let Some(required) = self.missing_set(id.set, learner.completed) {
            return Access::NeedsSet(required);
        }
        match set.kind {
            Kind::Practices => match lesson.opens_after {
                Some(after)
                    if !self
                        .lesson(after)
                        .is_some_and(|after| learner.completed.contains(&after.key)) =>
                {
                    Access::NeedsLesson(after)
                }
                _ => Access::Open,
            },
            Kind::Form => {
                let previous_done = id
                    .lesson
                    .checked_sub(1)
                    .and_then(|previous| set.lessons.get(previous))
                    .is_none_or(|previous| learner.completed.contains(&previous.key));
                match (done, previous_done) {
                    (true, _) => Access::Done,
                    (false, true) => Access::Open,
                    (false, false) => Access::NeedsPrevious,
                }
            }
        }
    }
}

/// Who is opening a lesson, as far as access to it goes.
pub struct Learner<'a> {
    pub member: bool,
    /// Keys ("set/lesson") of the lessons passed.
    pub completed: &'a HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Access {
    Open,
    /// Open, and passed already.
    Done,
    NeedsLogin,
    NeedsMembership,
    /// The lesson before it in the form isn't passed yet.
    NeedsPrevious,
    /// The set at this position is to be finished first.
    NeedsSet(usize),
    /// A practice waiting for this lesson to be passed.
    NeedsLesson(LessonId),
}

impl Access {
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Done)
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

    /// A content directory with `course` as course.toml and, for each set,
    /// its lessons.toml and texts for the lessons named, removed when dropped.
    struct TempContent(PathBuf);

    impl TempContent {
        fn new(
            name: &str,
            course: &str,
            sets: &[(&str, &str, &[&str])],
        ) -> Result<Self, std::io::Error> {
            let dir = std::env::temp_dir().join(format!(
                "taichichuan-mobi-course-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("course.toml"), course)?;
            for (set, lessons, slugs) in sets {
                std::fs::create_dir_all(dir.join(set))?;
                std::fs::write(dir.join(set).join("lessons.toml"), lessons)?;
                for slug in *slugs {
                    let lesson_dir = dir.join(set).join(slug);
                    std::fs::create_dir_all(&lesson_dir)?;
                    std::fs::write(lesson_dir.join("en.md"), "Text")?;
                    std::fs::write(lesson_dir.join("fi.md"), "Teksti")?;
                }
            }
            Ok(Self(dir))
        }
    }

    impl Drop for TempContent {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set(slug: &str, extra: &str) -> String {
        format!("[[set]]\nslug = \"{slug}\"\ntitle = {{ en = \"E\", fi = \"F\" }}\n{extra}\n")
    }

    fn lesson(slug: &str, extra: &str) -> String {
        format!("[[lesson]]\nslug = \"{slug}\"\ntitle = {{ en = \"E\", fi = \"F\" }}\n{extra}\n")
    }

    fn form_lesson(slug: &str, movements: &str, extra: &str) -> String {
        lesson(
            slug,
            &format!("movements = {movements}\npassing = \"practice\"\n{extra}"),
        )
    }

    #[test]
    fn loads_test_content() -> TestResult {
        let course = Course::load(Path::new("tests/lessons"))?;
        let sets: Vec<(&str, Kind, usize)> = course
            .sets()
            .iter()
            .map(|set| (set.slug.as_str(), set.kind, set.lessons.len()))
            .collect();
        assert_eq!(
            sets,
            [
                ("practices", Kind::Practices, 3),
                ("slow-set", Kind::Form, 3),
                ("fast-set", Kind::Form, 1)
            ]
        );
        let (id, whip) = course
            .find("slow-set", "single-whip")
            .ok_or("single-whip")?;
        assert_eq!(id, LessonId { set: 1, lesson: 2 });
        assert_eq!(whip.key, "slow-set/single-whip");
        assert_eq!(whip.movements, [5, 30]);
        assert_eq!(whip.passing, Some(Passing::Review));
        assert!(!whip.free);
        assert_eq!(whip.title(Language::Fi), "Yksittäinen ruoska");
        assert_eq!(course.sets()[1].title(Language::Fi), "Hidas sarja");
        assert!(course.find("slow-set", "missing").is_none());
        assert!(course.find("missing", "single-whip").is_none());
        let (_, push_hands) = course.find("practices", "push-hands").ok_or("push-hands")?;
        assert_eq!(push_hands.passing, None);
        assert_eq!(push_hands.opens_after, Some(LessonId { set: 1, lesson: 1 }));

        let (_, preparation) = course
            .find("slow-set", "preparation")
            .ok_or("preparation")?;
        assert_eq!(preparation.video(Language::En), None);
        let (_, opening) = course.find("slow-set", "opening").ok_or("opening")?;
        let video = Path::new("tests/lessons/slow-set/opening/video.mp4");
        assert_eq!(opening.video(Language::En), Some(video));
        assert_eq!(opening.video(Language::Fi), Some(video));
        // single-whip has a Finnish video only, shown in English too.
        let video = Path::new("tests/lessons/slow-set/single-whip/video-fi.mp4");
        assert_eq!(whip.video(Language::En), Some(video));
        assert_eq!(whip.video(Language::Fi), Some(video));
        Ok(())
    }

    #[test]
    fn lessons_open_by_membership_order_sets_and_practices() -> TestResult {
        use Access::{Done, NeedsLesson, NeedsMembership, NeedsPrevious, NeedsSet, Open};
        let course = Course::load(Path::new("tests/lessons"))?;
        // practices: standing (free), silk-reeling, push-hands (after
        // slow-set/opening); slow-set: preparation and opening (free),
        // single-whip; fast-set (requires slow-set): fast-opening.
        let access = |completed: &[&str], member: bool| {
            let completed: HashSet<String> =
                completed.iter().map(|key| (*key).to_string()).collect();
            let learner = Learner {
                member,
                completed: &completed,
            };
            course
                .sets()
                .iter()
                .enumerate()
                .flat_map(|(set, found)| {
                    (0..found.lessons.len()).map(move |lesson| LessonId { set, lesson })
                })
                .map(|id| course.access(id, Some(&learner)))
                .collect::<Vec<_>>()
        };
        let opening = NeedsLesson(LessonId { set: 1, lesson: 1 });
        let m = NeedsMembership;
        assert_eq!(access(&[], false), [Open, m, m, Open, Open, m, m]);
        assert_eq!(
            access(&["slow-set/opening"], false),
            [Open, m, m, Open, Done, m, m]
        );
        assert_eq!(
            access(&[], true),
            [Open, Open, opening, Open, Open, NeedsPrevious, NeedsSet(1)]
        );
        assert_eq!(
            access(&["slow-set/opening"], true),
            [Open, Open, Open, Open, Done, Open, NeedsSet(1)]
        );
        // Passed lessons stay open even if an earlier one isn't passed.
        assert_eq!(
            access(&["slow-set/single-whip"], true),
            [Open, Open, opening, Open, Open, Done, NeedsSet(1)]
        );
        // Finishing the slow set opens the fast set.
        let all = [
            "slow-set/preparation",
            "slow-set/opening",
            "slow-set/single-whip",
        ];
        assert_eq!(access(&all, true)[6], Open);
        let all: HashSet<String> = all.iter().map(|key| (*key).to_string()).collect();
        assert_eq!(course.passed(1, &all), 3);
        assert_eq!(course.missing_set(2, &all), None);
        assert_eq!(course.missing_set(2, &HashSet::new()), Some(1));
        assert_eq!(
            course.access(LessonId { set: 1, lesson: 0 }, None),
            Access::NeedsLogin
        );
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

    /// A set's slug, its lessons.toml, and the lessons to write texts for.
    type SetFiles<'a> = (&'a str, String, &'a [&'a str]);

    #[test]
    #[expect(clippy::too_many_lines, reason = "one case per mistake")]
    fn rejects_mistakes_in_the_content() -> TestResult {
        let form = set("a", "movements = 89");
        let practices = set("p", "practices = true");
        let x: &[&str] = &["x"];
        let cases: Vec<(&str, String, Vec<SetFiles>, &str)> = vec![
            (
                "set-slug",
                set("Bad Set", "movements = 89"),
                vec![],
                "a slug may only",
            ),
            (
                "set-twice",
                form.clone() + &form,
                vec![("a", form_lesson("x", "[1]", ""), x)],
                "set \"a\" is listed twice",
            ),
            (
                "no-kind",
                set("a", ""),
                vec![],
                "give the number of movements",
            ),
            (
                "practice-movements",
                set("p", "practices = true\nmovements = 3"),
                vec![],
                "practices have no movements",
            ),
            (
                "requires-later",
                set("a", "movements = 89\nrequires = [\"b\"]") + &set("b", "movements = 3"),
                vec![],
                "isn't listed above it",
            ),
            (
                "requires-practices",
                practices.clone() + &set("a", "movements = 89\nrequires = [\"p\"]"),
                vec![("p", lesson("x", ""), x)],
                "can't be finished",
            ),
            (
                "no-lessons",
                form.clone(),
                vec![("a", String::new(), &[])],
                "has no lessons",
            ),
            ("no-lessons-file", form.clone(), vec![], "lessons.toml"),
            (
                "lesson-slug",
                form.clone(),
                vec![("a", form_lesson("Bad", "[1]", ""), &["Bad"])],
                "a slug may only",
            ),
            (
                "lesson-twice",
                form.clone(),
                vec![(
                    "a",
                    form_lesson("x", "[1]", "") + &form_lesson("x", "[2]", ""),
                    x,
                )],
                "\"a/x\" is listed twice",
            ),
            (
                "range",
                set("a", "movements = 3"),
                vec![("a", form_lesson("x", "[4]", ""), x)],
                "not between 1 and 3",
            ),
            (
                "zero",
                form.clone(),
                vec![("a", form_lesson("x", "[0]", ""), x)],
                "not between 1 and 89",
            ),
            (
                "no-movements",
                form.clone(),
                vec![("a", form_lesson("x", "[]", ""), x)],
                "no movements",
            ),
            (
                "shared",
                form.clone(),
                vec![(
                    "a",
                    form_lesson("x", "[1, 2]", "") + &form_lesson("y", "[2]", ""),
                    &["x", "y"],
                )],
                "movement 2 is already",
            ),
            (
                "no-passing",
                form.clone(),
                vec![("a", lesson("x", "movements = [1]"), x)],
                "say how it is passed",
            ),
            (
                "passing",
                form.clone(),
                vec![(
                    "a",
                    form_lesson("x", "[1]", "").replace("practice", "exam"),
                    x,
                )],
                "exam",
            ),
            (
                "free-later",
                form.clone(),
                vec![(
                    "a",
                    form_lesson("x", "[1]", "") + &form_lesson("y", "[2]", "free = true"),
                    &["x", "y"],
                )],
                "free lessons come first",
            ),
            (
                "free-required",
                form.clone() + &set("b", "movements = 3\nrequires = [\"a\"]"),
                vec![
                    ("a", form_lesson("x", "[1]", ""), x),
                    ("b", form_lesson("y", "[1]", "free = true"), &["y"]),
                ],
                "its set requires another set",
            ),
            (
                "form-opens-after",
                form.clone(),
                vec![("a", form_lesson("x", "[1]", "opens_after = \"a/x\""), x)],
                "without opens_after",
            ),
            (
                "practice-passing",
                practices.clone(),
                vec![("p", lesson("x", "passing = \"quiz\""), x)],
                "practices have no movements or passing",
            ),
            (
                "free-waiting",
                practices.clone() + &form,
                vec![
                    ("p", lesson("x", "free = true\nopens_after = \"a/y\""), x),
                    ("a", form_lesson("y", "[1]", ""), &["y"]),
                ],
                "can't wait",
            ),
            (
                "opens-after-missing",
                practices.clone(),
                vec![("p", lesson("x", "opens_after = \"a/y\""), x)],
                "isn't a lesson",
            ),
            (
                "opens-after-practice",
                practices.clone(),
                vec![(
                    "p",
                    lesson("x", "") + &lesson("y", "opens_after = \"p/x\""),
                    &["x", "y"],
                )],
                "is a practice",
            ),
            (
                "missing-text",
                form.clone(),
                vec![("a", form_lesson("x", "[1]", ""), &[])],
                "en.md",
            ),
            (
                "unknown-field",
                form.clone(),
                vec![("a", form_lesson("x", "[1]", "vidoe = \"x\""), x)],
                "vidoe",
            ),
            (
                "empty-title",
                form.clone(),
                vec![(
                    "a",
                    form_lesson("x", "[1]", "").replace("en = \"E\"", "en = \" \""),
                    x,
                )],
                "a title is empty",
            ),
        ];
        for (name, course, sets, expected) in &cases {
            let sets: Vec<(&str, &str, &[&str])> = sets
                .iter()
                .map(|(set, lessons, slugs)| (*set, lessons.as_str(), *slugs))
                .collect();
            let content = TempContent::new(name, course, &sets)?;
            let Err(error) = Course::load(&content.0) else {
                return Err(format!("{name}: loaded").into());
            };
            assert!(error.contains(expected), "{name}: {error}");
        }
        Ok(())
    }

    #[test]
    fn missing_course_file_names_it() -> TestResult {
        let error = Course::load(Path::new("tests/no-such-dir"))
            .err()
            .ok_or("loaded")?;
        assert!(error.contains("course.toml"), "{error}");
        Ok(())
    }
}
