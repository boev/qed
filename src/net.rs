pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

pub async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
        return Err("response exceeded the 4 MiB limit".to_owned());
    }
    let mut output = Vec::new();
    while let Some(chunk) =
        response.chunk().await.map_err(|_| "response body read failed".to_owned())?
    {
        if output.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("response exceeded the 4 MiB limit".to_owned());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}
/// Parse an HTTPS URL only when it targets the expected external service host.
pub fn allowlisted_https_url(url: &str, host: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::allowlisted_https_url;

    #[test]
    fn allowlists_only_the_exact_https_host() {
        assert!(allowlisted_https_url("https://sourcify.dev/server/v2", "sourcify.dev").is_some());
        assert!(allowlisted_https_url("https://sourcify.dev.evil.example/path", "sourcify.dev").is_none());
        assert!(allowlisted_https_url("http://sourcify.dev/path", "sourcify.dev").is_none());
        assert!(allowlisted_https_url("https://user@sourcify.dev/path", "sourcify.dev").is_none());
        assert!(allowlisted_https_url("https://sourcify.dev:8443/path", "sourcify.dev").is_none());
    }
}
