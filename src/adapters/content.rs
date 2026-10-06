use chrono::{NaiveDate, SecondsFormat, Utc};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::{fs, io, path::Path};

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");
const SECURITY: &str = include_str!("../../SECURITY.md");

pub(crate) struct ChangelogEntry {
    pub(crate) anchor: String,
    version: String,
    date: Option<String>,
    pub(crate) headline: String,
    summary_html: String,
    details_html: Option<String>,
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

fn markdown_events(source: &str) -> Vec<Event<'_>> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    Parser::new_ext(source, options)
        .map(|event| match event {
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            other => other,
        })
        .collect()
}

fn render_events<'a>(events: impl IntoIterator<Item = Event<'a>>) -> String {
    let mut rendered = String::new();
    pulldown_cmark::html::push_html(&mut rendered, events.into_iter());
    rendered
}

pub(crate) fn render_markdown(source: &str) -> String {
    render_events(markdown_events(source))
}

fn render_blog_markdown(source: &str) -> String {
    let events = markdown_events(source);
    let paragraphs = top_level_paragraphs(&events);
    let mut transformed = Vec::with_capacity(events.len());
    let mut paragraph_index = 0;
    let mut cursor = 0;

    while paragraph_index < paragraphs.len() {
        let (start, end) = paragraphs[paragraph_index];
        transformed.extend(events[cursor..start].iter().cloned());
        if let Some(image) = image_paragraph(&events, start, end) {
            transformed.push(Event::Html(blog_image_figure(&image).into()));
            cursor = end + 1;
            paragraph_index += 1;
        } else if let Some((next_start, next_end)) = paragraphs.get(paragraph_index + 1).copied()
            && next_start == end + 1
            && let Some(image) = image_paragraph(&events, next_start, next_end)
        {
            transformed.push(Event::Html(
                "<div class=\"blog-step\"><div class=\"blog-step-copy\">".into(),
            ));
            transformed.extend(events[start..=end].iter().cloned());
            transformed.push(Event::Html("</div>".into()));
            transformed.push(Event::Html(blog_image_figure(&image).into()));
            transformed.push(Event::Html("</div>".into()));
            cursor = next_end + 1;
            paragraph_index += 2;
        } else {
            transformed.extend(events[start..=end].iter().cloned());
            cursor = end + 1;
            paragraph_index += 1;
        }
    }

    transformed.extend(events[cursor..].iter().cloned());
    render_events(transformed)
}

fn top_level_paragraphs(events: &[Event<'_>]) -> Vec<(usize, usize)> {
    let mut paragraphs = Vec::new();
    let mut depth = 0usize;
    let mut index = 0;
    while index < events.len() {
        if depth == 0 && matches!(events.get(index), Some(Event::Start(Tag::Paragraph))) {
            let start = index;
            let mut paragraph_depth = 0usize;
            let end = events[index..].iter().enumerate().find_map(|(offset, event)| {
                match event {
                    Event::Start(_) => paragraph_depth += 1,
                    Event::End(_) => {
                        paragraph_depth -= 1;
                        if paragraph_depth == 0 {
                            return Some(start + offset);
                        }
                    }
                    _ => {}
                }
                None
            });
            if let Some(end) = end {
                paragraphs.push((start, end));
                index = end + 1;
                continue;
            }
        }
        match &events[index] {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    paragraphs
}

struct BlogImage {
    url: String,
    alt: String,
    title: String,
}

fn image_paragraph(events: &[Event<'_>], start: usize, end: usize) -> Option<BlogImage> {
    if !matches!(events.get(start), Some(Event::Start(Tag::Paragraph)))
        || !matches!(events.get(end), Some(Event::End(TagEnd::Paragraph)))
    {
        return None;
    }
    let mut image_start = start + 1;
    while image_start < end && is_markdown_whitespace(&events[image_start]) {
        image_start += 1;
    }
    let Event::Start(Tag::Image { dest_url, title, .. }) = events.get(image_start)? else {
        return None;
    };
    let url = dest_url.to_string();
    let title = title.to_string();
    let mut alt = String::new();
    let mut image_end = None;
    for (offset, event) in events[image_start + 1..end].iter().enumerate() {
        match event {
            Event::Text(value) | Event::Code(value) => alt.push_str(value),
            Event::SoftBreak | Event::HardBreak => alt.push(' '),
            Event::End(TagEnd::Image) => {
                image_end = Some(image_start + 1 + offset);
                break;
            }
            _ => {}
        }
    }
    let image_end = image_end?;
    if events[image_end + 1..end].iter().any(|event| !is_markdown_whitespace(event)) {
        return None;
    }
    Some(BlogImage { url, alt, title })
}

fn is_markdown_whitespace(event: &Event<'_>) -> bool {
    matches!(event, Event::Text(value) if value.trim().is_empty())
        || matches!(event, Event::SoftBreak | Event::HardBreak)
}

fn blog_image_figure(image: &BlogImage) -> String {
    let url = html_escape(&image.url);
    let alt = html_escape(&image.alt);
    let title =
        (!image.title.is_empty()).then(|| format!(" title=\"{}\"", html_escape(&image.title)));
    let title = title.as_deref().unwrap_or_default();
    let light_link = blog_image_link(&url, &alt, title, " blog-image-light");
    let image_links = if let Some(dark_url) = dark_sibling_image(&image.url) {
        format!(
            "{light_link}{}",
            blog_image_link(&html_escape(&dark_url), &alt, title, " blog-image-dark")
        )
    } else {
        blog_image_link(&url, &alt, title, "")
    };
    format!("<figure class=\"blog-figure\">{image_links}</figure>")
}

fn blog_image_link(url: &str, alt: &str, title: &str, class: &str) -> String {
    format!(
        "<a class=\"blog-image-link{class}\" href=\"{url}\" target=\"_blank\" rel=\"noopener noreferrer\"><img src=\"{url}\" alt=\"{alt}\" loading=\"lazy\"{title}></a>"
    )
}

fn dark_sibling_image(url: &str) -> Option<String> {
    let relative = url.strip_prefix("/static/blog/")?;
    let (date, filename) = relative.split_once('/')?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    if filename.contains('/') || filename.contains('?') || filename.contains('#') {
        return None;
    }
    let stem = filename.strip_suffix(".png")?;
    if stem.is_empty() || stem.ends_with("-dark") {
        return None;
    }
    let dark_filename = format!("{stem}-dark.png");
    Path::new("static/blog")
        .join(date)
        .join(&dark_filename)
        .is_file()
        .then(|| format!("/static/blog/{date}/{dark_filename}"))
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
    let mut current: Option<(String, Option<String>, String)> = None;

    for line in source.lines() {
        if let Some(header) = line.strip_prefix("## [Release ") {
            finish_changelog_entry(&mut entries, current.take());
            let Some((version, date_label)) = header.split_once(']') else {
                continue;
            };
            let version = version.trim();
            let mut components = version.split('.');
            let major_valid = components.next().is_some_and(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            });
            let minor_valid = components.next().is_none_or(|part| {
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            }) && components.next().is_none();
            if !major_valid || !minor_valid {
                continue;
            }
            let date_label = date_label.trim().trim_start_matches('-').trim();
            if date_label.eq_ignore_ascii_case("Unreleased") {
                continue;
            }
            current = Some((version.to_owned(), Some(date_label.to_owned()), String::new()));
        } else if let Some((_, _, body)) = &mut current {
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
    current: Option<(String, Option<String>, String)>,
) {
    if let Some((version, date, body)) = current {
        let (short_source, details_source) = split_changelog_details(&body);
        let mut headline = None;
        let mut summary_markdown = String::new();
        for line in short_source.lines() {
            if headline.is_none() {
                if let Some(value) = line
                    .trim()
                    .strip_prefix("**")
                    .and_then(|value| value.strip_suffix("**"))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    headline = Some(value.to_owned());
                    continue;
                }
            }
            summary_markdown.push_str(line);
            summary_markdown.push('\n');
        }
        let headline = headline.unwrap_or_else(|| format!("Release {version}"));
        let summary_html = render_markdown(summary_markdown.trim());
        let details_html = details_source
            .map(str::trim)
            .filter(|details| !details.is_empty())
            .map(render_markdown);
        entries.push(ChangelogEntry {
            anchor: format!("release-{version}"),
            version,
            date,
            headline,
            summary_html,
            details_html,
        });
    }
}

fn split_changelog_details(body: &str) -> (&str, Option<&str>) {
    body.split_once("\n### Details\n")
        .map(|(short, details)| (short.trim(), Some(details.trim())))
        .unwrap_or((body.trim(), None))
}

fn changelog_entry_html(entry: &ChangelogEntry) -> String {
    let date_html = entry
        .date
        .as_deref()
        .map(|date| {
            format!(
                "<time class=\"release-date\" datetime=\"{}\">{}</time>",
                html_escape(date),
                html_escape(date)
            )
        })
        .unwrap_or_default();
    let details_html = entry
        .details_html
        .as_deref()
        .map(|details| {
            format!(
                "<details class=\"changelog-details\"><summary>Show details</summary>{details}</details>"
            )
        })
        .unwrap_or_default();
    format!(
        "<section class=\"changelog-entry\" id=\"{}\"><header class=\"changelog-entry-meta\"><span class=\"release-version\">Release {}</span>{date_html}</header><h2><a href=\"#{}\">{}</a></h2>{}{details_html}</section>",
        html_escape(&entry.anchor),
        html_escape(&entry.version),
        html_escape(&entry.anchor),
        html_escape(&entry.headline),
        entry.summary_html,
    )
}

pub(crate) fn changelog_html() -> String {
    changelog_entries().iter().map(changelog_entry_html).collect()
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
                xml_escape(&entry.headline),
                xml_escape(&link),
                xml_escape(&link),
                xml_escape(&updated),
                xml_escape(&entry.summary_html),
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

    let body_html = render_blog_markdown(markdown.trim());
    Ok(BlogPost { slug, title, date, summary, draft, poster, video, body_html })
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
        "<article class=\"blog-article\"><p class=\"blog-date\">{}</p>{poster}{}</article>",
        html_escape(&post.date),
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
        assert_eq!(entries[0].anchor, "release-8.1");
        assert_eq!(entries[0].headline, "Pages open at the top; changelog gets shorter.");
        let page = changelog_html();
        let feed = changelog_atom("https://qed.example");
        assert!(page.contains("id=\"release-8.1\""));
        assert!(page.contains("id=\"release-1\""));
        assert!(page.contains("Release 8.1"));
        assert!(!page.contains("Unreleased"));
        assert!(!feed.contains("Unreleased"));
        assert_eq!(feed.matches("<entry>").count(), 9);
    }
    #[test]
    fn changelog_details_are_collapsed_and_excluded_from_the_atom_feed() {
        let source = "## [Release 8.1] - 2026-10-06\n\n**Short headline**\n\n- **Feature** — short summary.\n\n### Details\n\nLong implementation detail.";
        let entry = parse_changelog_entries(source).into_iter().next().expect("parsed release");
        assert_eq!(entry.version, "8.1");
        assert_eq!(entry.headline, "Short headline");
        assert!(entry.summary_html.contains("<strong>Feature</strong>"));
        assert!(!entry.summary_html.contains("Long implementation detail"));
        assert!(
            entry
                .details_html
                .as_deref()
                .is_some_and(|details| details.contains("Long implementation detail"))
        );
        let rendered = changelog_entry_html(&entry);
        assert!(
            rendered
                .contains("<details class=\"changelog-details\"><summary>Show details</summary>")
        );
        assert!(!rendered.contains("<h3>Details</h3>"));

        let page = changelog_html();
        let feed = changelog_atom("https://qed.example");
        assert!(page.contains("Guard reviews supported-chain tokens and recognized pools"));
        assert!(!feed.contains("Guard reviews supported-chain tokens and recognized pools"));
        assert!(feed.contains("one signed review covers issuer identity"));
        assert!(!feed.contains("Statements record observed balances at a block height"));
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
    fn blog_markdown_images_render_as_linked_step_figures() {
        let post = parse_blog_post(
            "2026-10-06-image-post.md",
            "---\ntitle: Image post\ndate: 2026-10-06\nsummary: Image example\ndraft: false\n---\n\nStep description.\n\n![A QED example](/static/blog/2026-10-06/example.png)",
        )
        .expect("valid image post");
        let rendered = blog_post_html(&post);
        assert!(!rendered.contains("class=\"blog-summary\""));
        assert!(rendered.contains("<div class=\"blog-step\"><div class=\"blog-step-copy\">"));
        assert!(rendered.contains("<p>Step description.</p>"));
        assert!(rendered.contains("<figure class=\"blog-figure\">"));
        assert!(rendered.contains("<a class=\"blog-image-link\" href=\"/static/blog/2026-10-06/example.png\" target=\"_blank\" rel=\"noopener noreferrer\">"));
        assert!(
            rendered
                .contains("<img src=\"/static/blog/2026-10-06/example.png\" alt=\"A QED example\"")
        );
        assert!(rendered.contains("</figure></div></article>"));
        assert_eq!(rendered.matches("class=\"blog-image-link").count(), 1);
        assert!(!rendered.contains("blog-image-dark"));
    }

    #[test]
    fn blog_images_use_available_dark_siblings() {
        let post = parse_blog_post(
            "2026-10-04-image-post.md",
            "---\ntitle: Image post\ndate: 2026-10-04\nsummary: Image example\ndraft: false\n---\n\n![QED screenshot](/static/blog/2026-10-06/1-guard.png)",
        )
        .expect("valid image post");
        let rendered = blog_post_html(&post);
        assert!(rendered.contains(
            "class=\"blog-image-link blog-image-light\" href=\"/static/blog/2026-10-06/1-guard.png\""
        ));
        assert!(rendered.contains(
            "class=\"blog-image-link blog-image-dark\" href=\"/static/blog/2026-10-06/1-guard-dark.png\""
        ));
        assert!(rendered.contains("src=\"/static/blog/2026-10-06/1-guard-dark.png\""));
    }

    #[test]
    fn published_blog_screenshot_paragraphs_form_three_steps() {
        let posts = blog_posts().expect("blog source files");
        let post = posts
            .iter()
            .find(|post| post.slug == "three-ways-to-protect-funds")
            .expect("published screenshot article");
        let rendered = blog_post_html(post);
        assert_eq!(rendered.matches("<div class=\"blog-step\">").count(), 3);
        assert_eq!(rendered.matches("<figure class=\"blog-figure\">").count(), 3);
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
