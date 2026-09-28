use super::{ASSET_VERSION, render_page, wants_fragment};
use crate::state::AppState;
use askama::Template;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Html,
};

pub(crate) async fn imprint(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    legal_page(
        &state,
        &headers,
        "Imprint",
        "/imprint",
        include_str!("../../release/legal/imprint.md"),
    )
}

pub(crate) async fn privacy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    legal_page(
        &state,
        &headers,
        "Privacy",
        "/privacy",
        include_str!("../../release/legal/privacy.md"),
    )
}

pub(crate) async fn terms(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<String>, StatusCode> {
    legal_page(&state, &headers, "Terms", "/terms", include_str!("../../release/legal/terms.md"))
}

fn legal_page(
    state: &AppState,
    headers: &HeaderMap,
    title: &str,
    canonical_path: &str,
    source: &str,
) -> Result<Html<String>, StatusCode> {
    render_page(
        LegalTemplate {
            asset_version: ASSET_VERSION,
            public_url: state.public_url.to_string(),
            title: title.to_owned(),
            canonical_path: canonical_path.to_owned(),
            body: markdownish(source),
        },
        wants_fragment(headers),
    )
}
#[derive(Debug, Template)]
#[template(path = "legal.html")]
struct LegalTemplate {
    asset_version: u64,
    public_url: String,
    title: String,
    canonical_path: String,
    body: String,
}

fn markdownish(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            if let Some(title) = line.strip_prefix("# ") {
                format!("<h1 class=\"page-title\">{title}</h1>")
            } else if let Some(title) = line.strip_prefix("## ") {
                format!("<h2>{title}</h2>")
            } else if line.is_empty() {
                String::new()
            } else {
                format!("<p>{line}</p>")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
