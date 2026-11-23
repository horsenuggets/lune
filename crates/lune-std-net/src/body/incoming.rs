use std::error::Error;

use http_body_util::BodyExt;
use hyper::{
    HeaderMap, Method, StatusCode,
    body::{Body, Bytes},
    header::{CONTENT_ENCODING, CONTENT_LENGTH, TRANSFER_ENCODING},
};

use mlua::prelude::*;

use lune_std_serde::{CompressDecompressFormat, decompress};

/**
    Returns the number of body bytes that the given headers declare, if any.

    A transfer encoding takes precedence over a content length, as per the HTTP
    specification, so there is no declared length when both are present.
*/
pub fn declared_body_length(headers: &HeaderMap) -> Option<u64> {
    if headers.contains_key(TRANSFER_ENCODING) {
        return None;
    }
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/**
    Returns the number of body bytes that a response must contain, if any.

    Responses to `HEAD` requests and responses with informational, `204` or
    `304` statuses never contain a body, even when they declare a length.
*/
pub fn expected_response_body_length(
    method: &Method,
    status: StatusCode,
    headers: &HeaderMap,
) -> Option<u64> {
    let bodiless = method == Method::HEAD
        || status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED;
    if bodiless {
        None
    } else {
        declared_body_length(headers)
    }
}

/**
    Collects a full incoming body, decompressing it if requested.

    A body that ends before its expected length is an error, so a connection
    that stops early can never be mistaken for a complete, successful body.
*/
pub async fn handle_incoming_body<B>(
    headers: &HeaderMap,
    body: B,
    should_decompress: bool,
    expected_length: Option<u64>,
) -> LuaResult<(Bytes, bool)>
where
    B: Body<Data = Bytes>,
    B::Error: Error + Send + Sync + 'static,
{
    let mut body = body.collect().await.into_lua_err()?.to_bytes();

    if let Some(expected) = expected_length {
        let received = body.len() as u64;
        if received < expected {
            return Err(LuaError::runtime(format!(
                "The body ended after {received} of the {expected} bytes its \
                Content-Length header declared"
            )));
        }
    }

    let was_decompressed = if should_decompress {
        let decompress_format = headers
            .get(CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .and_then(CompressDecompressFormat::detect_from_header_str);
        if let Some(format) = decompress_format {
            body = Bytes::from(decompress(body, format).await?);
            true
        } else {
            false
        }
    } else {
        false
    };

    Ok((body, was_decompressed))
}

#[cfg(test)]
mod tests {
    use http_body_util::Full;
    use hyper::header::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn collect(map: &HeaderMap, data: &'static str) -> LuaResult<(Bytes, bool)> {
        let expected = declared_body_length(map);
        async_io::block_on(handle_incoming_body(
            map,
            Full::new(Bytes::from_static(data.as_bytes())),
            false,
            expected,
        ))
    }

    #[test]
    fn rejects_a_body_shorter_than_its_content_length() {
        let map = headers(&[("content-length", "10")]);
        let error = collect(&map, "short").unwrap_err().to_string();
        assert!(error.contains("5 of the 10 bytes"), "{error}");
    }

    #[test]
    fn accepts_a_body_matching_its_content_length() {
        let map = headers(&[("content-length", "5")]);
        assert_eq!(
            collect(&map, "exact").unwrap().0,
            Bytes::from_static(b"exact")
        );
    }

    #[test]
    fn ignores_content_length_when_a_transfer_encoding_is_present() {
        let map = headers(&[("content-length", "10"), ("transfer-encoding", "chunked")]);
        assert_eq!(declared_body_length(&map), None);
        assert!(collect(&map, "short").is_ok());
    }

    #[test]
    fn expects_no_body_for_head_requests_or_bodiless_statuses() {
        let map = headers(&[("content-length", "10")]);
        let get = Method::GET;
        assert_eq!(
            expected_response_body_length(&get, StatusCode::OK, &map),
            Some(10)
        );
        assert_eq!(
            expected_response_body_length(&Method::HEAD, StatusCode::OK, &map),
            None
        );
        for status in [
            StatusCode::CONTINUE,
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
        ] {
            assert_eq!(expected_response_body_length(&get, status, &map), None);
        }
    }
}
