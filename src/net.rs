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
