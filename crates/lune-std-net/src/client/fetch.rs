use std::{
    any::Any,
    collections::HashMap,
    panic::AssertUnwindSafe,
    str::FromStr,
    sync::{Arc, Mutex, PoisonError},
};

use async_executor::Executor;
use futures_lite::FutureExt;
use http_body_util::Full;
use hyper::{
    Method, Request as HyperRequest,
    body::Bytes,
    client::conn::http1::handshake,
    header::{ACCEPT, CONTENT_LENGTH, HeaderName, HeaderValue, USER_AGENT},
};

use url::Url;

use crate::{
    client::stream::HttpStream,
    shared::{
        hyper::HyperIo,
        request::{Request, prepare_outgoing_request},
        response::Response,
    },
};

/**
    Sends a simple request and returns the final response.

    This will follow any redirects returned by the server,
    modifying the request method and body as necessary.

    # WARNING

    This is an API meant only for private consumption by the main `lune`
    crate - unlike other functions in *this* crate, it is NOT guaranteed
    to follow semver or be otherwise stable outside of the private usage.
*/
#[doc(hidden)]
#[allow(clippy::implicit_hasher)]
pub async fn fetch(
    url: Url,
    method: Option<Method>,
    headers: Option<HashMap<String, String>>,
    body: Option<Vec<u8>>,
) -> Result<Response, String> {
    let body = match body {
        Some(body) => Bytes::from(body),
        None => Bytes::new(),
    };

    let mut request = HyperRequest::new(body);
    *request.uri_mut() = url.to_string().parse().unwrap();
    if let Some(method) = method {
        *request.method_mut() = method;
    }
    if let Some(headers) = headers {
        for (key, val) in headers {
            let key = HeaderName::from_str(key.as_str()).map_err(|e| e.to_string())?;
            let val = HeaderValue::from_str(val.as_str()).map_err(|e| e.to_string())?;
            request.headers_mut().insert(key, val);
        }
    }

    // Some headers are required by most if not
    // all servers, make sure those are present...
    if !request.headers().contains_key(USER_AGENT.as_str()) {
        let ua = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
        let ua = HeaderValue::from_str(ua).unwrap();
        request.headers_mut().insert(USER_AGENT, ua);
    }
    if !request.headers().contains_key(CONTENT_LENGTH.as_str()) && request.method() != Method::GET {
        let len = request.body().len().to_string();
        let len = HeaderValue::from_str(&len).unwrap();
        request.headers_mut().insert(CONTENT_LENGTH, len);
    }
    if !request.headers().contains_key(ACCEPT.as_str()) {
        let accept = HeaderValue::from_static("*/*");
        request.headers_mut().insert(ACCEPT, accept);
    }

    // ... we can now safely continue and send the request
    let mut req = Request::from(request);
    req.decompress = true;

    let exec = Executor::new();
    let fut = fetch_inner(&exec, url, req);
    exec.run(fut).await
}

/**
    The first panic of a connection driver, if any driver of a request panicked.

    The executor discards panics of detached tasks, and a driver that panics while a
    body is streaming drops its body sender, which reads as a normal end of the body.
    Recording the panic lets the request fail instead of returning a truncated body.
*/
type DriverPanic = Arc<Mutex<Option<String>>>;

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "an unknown panic payload".to_string()
    }
}

/**
    Spawns a connection driver that records a panic instead of letting it vanish.
*/
fn spawn_driver<F>(exec: &Executor<'_>, driver: F, panic: &DriverPanic)
where
    F: Future + Send + 'static,
{
    let panic = Arc::clone(panic);
    exec.spawn(async move {
        if let Err(payload) = AssertUnwindSafe(driver).catch_unwind().await {
            let mut slot = panic.lock().unwrap_or_else(PoisonError::into_inner);
            slot.get_or_insert_with(|| panic_message(payload.as_ref()));
        }
    })
    .detach();
}

/**
    Fails a request whose connection driver panicked, whatever its result was.
*/
fn finish<T>(result: Result<T, String>, panic: &DriverPanic) -> Result<T, String> {
    let slot = panic.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_ref() {
        Some(message) => Err(format!(
            "The connection failed while sending the request. {message}"
        )),
        None => result,
    }
}

async fn fetch_inner(exec: &Executor<'_>, url: Url, request: Request) -> Result<Response, String> {
    let panic = DriverPanic::default();
    let result = send_with_redirects(exec, url, request, &panic).await;
    finish(result, &panic)
}

async fn send_with_redirects(
    exec: &Executor<'_>,
    mut url: Url,
    mut request: Request,
    panic: &DriverPanic,
) -> Result<Response, String> {
    loop {
        let stream = HttpStream::connect_url(url.clone())
            .await
            .map_err(|e| e.to_string())?;

        let (mut sender, conn) = handshake(HyperIo::from(stream))
            .await
            .map_err(|e| e.to_string())?;

        spawn_driver(exec, conn, panic);

        let mut outgoing = request.clone_inner();
        prepare_outgoing_request(&mut outgoing);

        let (parts, body) = outgoing.into_parts();
        let data = HyperRequest::from_parts(parts, Full::new(body.into_bytes()));
        let incoming = sender.send_request(data).await.map_err(|e| e.to_string())?;

        if super::try_follow_redirect(&mut url, &mut request, &incoming)
            .map_err(ToString::to_string)?
        {
            continue;
        }

        break Response::from_incoming(incoming, &request.method(), request.decompress)
            .await
            .map_err(|e| e.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_driver<F>(driver: F) -> DriverPanic
    where
        F: Future + Send + 'static,
    {
        let exec = Executor::new();
        let panic = DriverPanic::default();
        spawn_driver(&exec, driver, &panic);
        async_io::block_on(exec.run(async {
            for _ in 0..4 {
                futures_lite::future::yield_now().await;
            }
        }));
        panic
    }

    #[test]
    fn fails_a_successful_response_when_its_driver_panicked() {
        let panic = run_driver(async { panic!("Driver panic sentinel") });
        let error = finish(Ok("complete-looking body"), &panic).unwrap_err();
        assert!(error.contains("Driver panic sentinel"), "{error}");
    }

    #[test]
    fn keeps_the_first_panic_of_several_drivers() {
        let exec = Executor::new();
        let panic = DriverPanic::default();
        spawn_driver(&exec, async { panic!("First driver") }, &panic);
        spawn_driver(&exec, async { panic!("Second driver") }, &panic);
        async_io::block_on(exec.run(async {
            for _ in 0..4 {
                futures_lite::future::yield_now().await;
            }
        }));
        let error = finish(Ok(()), &panic).unwrap_err();
        assert!(error.contains("First driver"), "{error}");
    }

    #[test]
    fn passes_results_through_when_no_driver_panicked() {
        let panic = run_driver(async {});
        assert_eq!(finish(Ok(7), &panic), Ok(7));
        assert_eq!(
            finish::<()>(Err("Refused".to_string()), &panic),
            Err("Refused".to_string())
        );
    }
}
