use chrono::{NaiveDate, SecondsFormat, Utc};
use pulldown_cmark::{Event, Options, Parser};
use std::{fs, io, path::Path};

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");
const SECURITY: &str = include_str!("../../SECURITY.md");

#[derive(Debug, Clone)]
pub(crate) struct ChangelogEntry {
    pub(crate) anchor: String,
    pub(crate) title: String,
    pub(crate) date: Option<String>,
    pub(crate) headline: String,
    pub(crate) body_html: String,
}

#[derive(Debug, Clone)]
pub(crate) struct BlogPost {
    pub(crate) slug: String,
    pub(crate) title: String,
    pub(crate) date: String,
    pub(crate) summary: String,
    pub(crate) draft: bool,
    poster: Option<String>,
    video: Option<String>,
    body_html: String,
}

pub(crate) fn render_markdown(source: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(source, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut rendered = String::new();
    pulldown_cmark::html::push_html(&mut rendered, parser);
    rendered
}

pub(crate) fn security_html() -> String {
    render_markdown(&without_first_heading(SECURITY))
}

fn without_first_heading(source: &str) -> String {
    let source = source.trim_start();
    if let Some((first, rest)) = source.split_once('\n')
        && first.starts_with("# ")
    {
        return rest.trim_start().to_owned();
    }
    source.to_owned()
}

pub(crate) fn changelog_entries() -> Vec<ChangelogEntry> {
    parse_changelog_entries(CHANGELOG)
}

fn parse_changelog_entries(source: &str) -> Vec<ChangelogEntry> {
    let mut entries = Vec::new();
    let mut current: Option<(String, String, Option<String>, String)> = None;

    for line in source.lines() {
        if let Some(header) = line.strip_prefix("## [Release ") {
            finish_changelog_entry(&mut entries, current.take());
            let Some((number, date_label)) = header.split_once(']') else {
                continue;
            };
            let date_label = date_label.trim().trim_start_matches('-').trim();
            let Ok(number) = number.parse::<u8>() else {
                continue;
            };
            if date_label.eq_ignore_ascii_case("Unreleased") {
                continue;
            }
            current = Some((
                format!("release-{number}"),
                format!("Release {number} — {date_label}"),
                Some(date_label.to_owned()),
                String::new(),
            ));
        } else if let Some((_, _, _, body)) = &mut current {
            body.push_str(line);
            body.push('\n');
        }
    }
    finish_changelog_entry(&mut entries, current);
    entries
}

pub(crate) fn latest_released_changelog_entry() -> io::Result<Option<ChangelogEntry>> {
    let source = fs::read_to_string("CHANGELOG.md")?;
    Ok(parse_changelog_entries(&source).into_iter().next())
}

fn finish_changelog_entry(
    entries: &mut Vec<ChangelogEntry>,
    current: Option<(String, String, Option<String>, String)>,
) {
    if let Some((anchor, title, date, body)) = current {
        let headline = body
            .lines()
            .find_map(|line| line.trim().strip_prefix("**")?.strip_suffix("**"))
            .map(str::trim)
            .filter(|headline| !headline.is_empty())
            .unwrap_or(&title)
            .to_owned();
        entries.push(ChangelogEntry {
            anchor,
            title,
            date,
            headline,
            body_html: render_markdown(body.trim()),
        });
    }
}

pub(crate) fn changelog_html() -> String {
    changelog_entries()
        .iter()
        .map(|entry| {
            format!(
                "<section class=\"changelog-entry\" id=\"{}\"><h2><a href=\"#{}\">{}</a></h2>{}</section>",
                html_escape(&entry.anchor),
                html_escape(&entry.anchor),
                html_escape(&entry.title),
                entry.body_html,
            )
        })
        .collect::<String>()
}

pub(crate) fn changelog_atom(base: &str) -> String {
    let entries = changelog_entries()
        .iter()
        .map(|entry| {
            let link = format!("{base}/changelog#{}", entry.anchor);
            let updated = entry
                .date
                .as_deref()
                .map(atom_date)
                .unwrap_or_else(current_atom_date);
            format!(
                "<entry><title>{}</title><id>{}</id><link href=\"{}\"/><updated>{}</updated><content type=\"html\">{}</content></entry>",
                xml_escape(&entry.title),
                xml_escape(&link),
                xml_escape(&link),
                xml_escape(&updated),
                xml_escape(&entry.body_html),
            )
        })
        .collect::<String>();
    atom_feed(base, "/changelog.xml", "QED changelog", &entries)
}

pub(crate) fn blog_posts() -> io::Result<Vec<BlogPost>> {
    let mut posts = Vec::new();
    for item in fs::read_dir("release/blog")? {
        let item = item?;
        if item.path().extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let file_name = item.file_name().into_string().map_err(|_| invalid_blog_file())?;
        let source = fs::read_to_string(item.path())?;
        posts.push(parse_blog_post(&file_name, &source)?);
    }
    posts
        .sort_by(|left, right| right.date.cmp(&left.date).then_with(|| left.slug.cmp(&right.slug)));
    Ok(posts)
}

pub(crate) fn published_blog_posts(posts: Vec<BlogPost>) -> Vec<BlogPost> {
    posts.into_iter().filter(|post| !post.draft).collect()
}
pub(crate) fn latest_published_blog_post(posts: &[BlogPost]) -> Option<&BlogPost> {
    posts.iter().find(|post| !post.draft)
}

fn parse_blog_post(file_name: &str, source: &str) -> io::Result<BlogPost> {
    let invalid = invalid_blog_file;
    let source = source.strip_prefix("---\n").ok_or_else(invalid)?;
    let (frontmatter, markdown) = source.split_once("\n---\n").ok_or_else(invalid)?;
    let mut fields = std::collections::HashMap::new();
    for line in frontmatter.lines() {
        let (key, value) = line.split_once(':').ok_or_else(invalid)?;
        fields.insert(key.trim().to_owned(), scalar(value));
    }

    let title = fields.remove("title").filter(|value| !value.is_empty()).ok_or_else(invalid)?;
    let date = fields.remove("date").filter(|value| !value.is_empty()).ok_or_else(invalid)?;
    NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| invalid())?;
    let summary = fields.remove("summary").filter(|value| !value.is_empty()).ok_or_else(invalid)?;
    let draft = match fields.remove("draft").as_deref() {
        Some("true") => true,
        Some("false") | None => false,
        _ => return Err(invalid()),
    };
    let poster = fields.remove("poster").filter(|value| !value.is_empty());
    if poster.as_deref().is_some_and(|value| {
        !value.starts_with("/static/")
            || value.contains("..")
            || value.chars().any(char::is_whitespace)
    }) {
        return Err(invalid());
    }
    let video = fields.remove("video").filter(|value| !value.is_empty());
    if video.as_deref().is_some_and(|value| {
        !(value.starts_with("https://") || value.starts_with("http://"))
            || value.chars().any(char::is_whitespace)
    }) {
        return Err(invalid());
    }
    let slug = slug_from_file_name(file_name)?;
    let filename_date = file_name.get(..10).ok_or_else(invalid)?;
    if filename_date != date {
        return Err(invalid());
    }

    Ok(BlogPost {
        slug,
        title,
        date,
        summary,
        draft,
        poster,
        video,
        body_html: render_markdown(markdown.trim()),
    })
}

fn scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn slug_from_file_name(file_name: &str) -> io::Result<String> {
    let invalid = invalid_blog_file;
    let stem =
        Path::new(file_name).file_stem().and_then(|value| value.to_str()).ok_or_else(invalid)?;
    if stem.len() < 12 || stem.as_bytes().get(10) != Some(&b'-') {
        return Err(invalid());
    }
    let date = &stem[..10];
    NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| invalid())?;
    let slug = &stem[11..];
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid());
    }
    Ok(slug.to_owned())
}

fn invalid_blog_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid QED blog front matter or filename")
}

pub(crate) fn blog_index_html(posts: &[BlogPost]) -> String {
    if posts.is_empty() {
        return "<p>No posts have been published.</p>".to_owned();
    }
    posts
        .iter()
        .filter(|post| !post.draft)
        .map(|post| {
            format!(
                "<article class=\"blog-listing\"><h2><a href=\"/blog/{}\">{}</a></h2><p class=\"blog-date\">{}</p><p>{}</p></article>",
                html_escape(&post.slug),
                html_escape(&post.title),
                html_escape(&post.date),
                html_escape(&post.summary),
            )
        })
        .collect::<String>()
}

pub(crate) fn blog_post_html(post: &BlogPost) -> String {
    let poster = match (&post.poster, &post.video) {
        (Some(poster), Some(video)) => format!(
            "<p class=\"blog-poster\"><a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\"><img src=\"{}\" alt=\"{}\" loading=\"lazy\"></a></p>",
            html_escape(video),
            html_escape(poster),
            html_escape(&post.title),
        ),
        (Some(poster), None) => format!(
            "<p class=\"blog-poster\"><img src=\"{}\" alt=\"{}\" loading=\"lazy\"></p>",
            html_escape(poster),
            html_escape(&post.title),
        ),
        _ => String::new(),
    };
    format!(
        "<p class=\"blog-date\">{}</p><p class=\"blog-summary\">{}</p>{poster}{}",
        html_escape(&post.date),
        html_escape(&post.summary),
        post.body_html,
    )
}

pub(crate) fn blog_atom(base: &str, posts: &[BlogPost]) -> String {
    let entries = posts
        .iter()
        .filter(|post| !post.draft)
        .map(|post| {
            let link = format!("{base}/blog/{}", post.slug);
            let updated = atom_date(&post.date);
            let body = blog_post_html(post);
            format!(
                "<entry><title>{}</title><id>{}</id><link href=\"{}\"/><updated>{}</updated><summary>{}</summary><content type=\"html\">{}</content></entry>",
                xml_escape(&post.title),
                xml_escape(&link),
                xml_escape(&link),
                xml_escape(&updated),
                xml_escape(&post.summary),
                xml_escape(&body),
            )
        })
        .collect::<String>();
    atom_feed(base, "/blog.xml", "QED blog", &entries)
}

fn atom_feed(base: &str, path: &str, title: &str, entries: &str) -> String {
    let feed_url = format!("{base}{path}");
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><feed xmlns=\"http://www.w3.org/2005/Atom\"><id>{}</id><title>{}</title><updated>{}</updated><link rel=\"self\" href=\"{}\"/><link href=\"{}\"/>{entries}</feed>",
        xml_escape(&feed_url),
        xml_escape(title),
        xml_escape(&current_atom_date()),
        xml_escape(&feed_url),
        xml_escape(base),
    )
}

fn atom_date(date: &str) -> String {
    format!("{date}T00:00:00Z")
}

fn current_atom_date() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(file_name: &str, draft: bool) -> BlogPost {
        let draft_value = if draft { "true" } else { "false" };
        parse_blog_post(
            file_name,
            &format!(
                "---\ntitle: Test post\ndate: 2026-10-04\nsummary: A summary\ndraft: {draft_value}\n---\n\n**Body**"
            ),
        )
        .expect("valid front matter")
    }

    #[test]
    fn markdown_escapes_raw_html_in_blog_content() {
        let rendered = render_markdown("<script>alert(1)</script>\n\n**contract**");
        assert!(!rendered.contains("<script>"));
        assert!(rendered.contains("&lt;script&gt;"));
        assert!(rendered.contains("<strong>contract</strong>"));
    }

    #[test]
    fn drafts_are_excluded_from_blog_index_and_atom_feed() {
        let posts = published_blog_posts(vec![
            post("2026-10-04-draft-post.md", true),
            post("2026-10-04-published-post.md", false),
        ]);
        assert_eq!(posts.len(), 1);
        let index = blog_index_html(&posts);
        let feed = blog_atom("https://qed.example", &posts);
        assert!(index.contains("/blog/published-post"));
        assert!(!index.contains("draft-post"));
        assert!(feed.contains("/blog/published-post"));
        assert!(!feed.contains("draft-post"));
        assert_eq!(feed.matches("<entry>").count(), 1);
    }

    #[test]
    fn released_changelog_entries_render_in_reverse_chronological_order() {
        let entries = changelog_entries();
        assert_eq!(entries[0].anchor, "release-8");
        assert_eq!(
            entries[0].headline,
            "QED Guard and Statement make issuer checks usable as signed evidence."
        );
        let page = changelog_html();
        let feed = changelog_atom("https://qed.example");
        assert!(page.contains("id=\"release-8\""));
        assert!(page.contains("id=\"release-1\""));
        assert!(!page.contains("Unreleased"));
        assert!(!feed.contains("Unreleased"));
        assert_eq!(feed.matches("<entry>").count(), 8);
    }

    #[test]
    fn latest_released_changelog_entry_skips_unreleased() {
        let source = "## [Release 9] - Unreleased\n\n**Work in progress**\n\n## [Release 8] - 2026-10-05\n\n**Released headline**\n";
        let entry = parse_changelog_entries(source).into_iter().next().expect("released entry");
        assert_eq!(entry.anchor, "release-8");
        assert_eq!(entry.headline, "Released headline");
    }

    #[test]
    fn latest_blog_link_skips_drafts() {
        let posts =
            [post("2026-10-04-draft-post.md", true), post("2026-10-04-published-post.md", false)];
        assert_eq!(
            latest_published_blog_post(&posts).map(|post| post.slug.as_str()),
            Some("published-post")
        );
    }

    #[test]
    fn blog_markdown_images_are_rendered_in_article_content() {
        let post = parse_blog_post(
            "2026-10-06-image-post.md",
            "---\ntitle: Image post\ndate: 2026-10-06\nsummary: Image example\ndraft: false\n---\n\n![A QED example](/static/blog/2026-10-06/example.png)",
        )
        .expect("valid image post");
        let rendered = blog_post_html(&post);
        assert!(rendered.contains("<img src=\"/static/blog/2026-10-06/example.png\""));
        assert!(rendered.contains("alt=\"A QED example\""));
    }

    #[test]
    fn poster_links_to_external_video_without_an_iframe() {
        let post = parse_blog_post(
            "2026-10-04-poster-post.md",
            "---\ntitle: Test post\ndate: 2026-10-04\nsummary: A summary\nposter: /static/poster.png\nvideo: https://video.example/watch\n---\n\nText.",
        )
        .expect("valid poster front matter");
        let rendered = blog_post_html(&post);
        assert!(rendered.contains("<a href=\"https://video.example/watch\""));
        assert!(rendered.contains("<img src=\"/static/poster.png\""));
        assert!(!rendered.contains("<iframe"));
    }
}
