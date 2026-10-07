// SPDX-License-Identifier: MIT OR Apache-2.0
//! The client: one pool of connections for the program (HTTP/1.1 and HTTP/2, TLS by rustls with the
//! roots of the Mozilla list), and a request that follows redirects by hand so that every hop is held
//! against the scope again: a server cannot send an application where its manifest does not reach.
use std::sync::{Arc, OnceLock};

use alef_core::{AlefError, ErrorCode};
use hyper::{
    body::Incoming,
    header::{
        HeaderValue, AUTHORIZATION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_LOCATION,
        CONTENT_TYPE, COOKIE, LOCATION, USER_AGENT,
    },
    HeaderMap, Method, Request, Response, StatusCode, Uri,
};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use url::Url;

use super::body::{Payload, RequestBody};

/// How many redirects a request follows before it gives up.
pub(super) const MAX_REDIRECTS: usize = 10;

type Pool = Client<HttpsConnector<HttpConnector>, RequestBody>;

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let connector = HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(provider)
            .expect("the provider supports the versions of TLS in use")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        Client::builder(TokioExecutor::new()).build(connector)
    })
}

/// What to ask for.
pub(super) struct Spec {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub follow: bool,
}

/// The answer, and where it came from after the redirects.
pub(super) struct Answer {
    pub response: Response<Incoming>,
    pub url: Url,
    pub redirected: bool,
}

/// Says whether the program may reach a URL; asked again for every hop.
pub(super) type Authorize = Box<dyn Fn(&str) -> Result<(), AlefError> + Send + Sync>;

fn network(message: &str) -> AlefError {
    AlefError::new(ErrorCode::Network, message)
}

/// Headers that describe a body: they go when the body goes.
const BODY_HEADERS: [hyper::header::HeaderName; 4] = [
    CONTENT_TYPE,
    CONTENT_ENCODING,
    CONTENT_LANGUAGE,
    CONTENT_LOCATION,
];

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// The next hop of a redirect: the URL, and whether the request goes on (`None`: the answer is the
/// redirect itself, because it names no place, is no redirect to follow, or the body cannot be sent
/// again). As the Fetch standard has it, a 303 turns any method but GET and HEAD into a GET, a 301 or a
/// 302 turns a POST into a GET, and the body goes with the method that is changed.
fn next_hop(
    status: StatusCode,
    location: Option<&HeaderValue>,
    url: &Url,
    method: &mut Method,
    headers: &mut HeaderMap,
    payload: &mut Payload,
) -> Result<Option<Url>, AlefError> {
    let Some(location) = location else {
        return Ok(None);
    };
    let to_get = match status {
        StatusCode::SEE_OTHER => *method != Method::GET && *method != Method::HEAD,
        StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => *method == Method::POST,
        StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => false,
        _ => return Ok(None),
    };
    if !to_get && !payload.replayable() {
        return Ok(None);
    }
    let location = location
        .to_str()
        .map_err(|_| network("the server redirected to an address that is not text"))?;
    let next = url
        .join(location)
        .map_err(|_| network("the server redirected to an address that is not valid"))?;
    if !matches!(next.scheme(), "http" | "https") {
        return Err(network(
            "the server redirected to something that is not HTTP",
        ));
    }
    if to_get {
        *method = Method::GET;
        *payload = Payload::Empty;
        for name in &BODY_HEADERS {
            headers.remove(name);
        }
    }
    if !same_origin(url, &next) {
        headers.remove(AUTHORIZATION);
        headers.remove(COOKIE);
    }
    Ok(Some(next))
}

/// Sends the request and follows the redirects the spec asks for, up to [`MAX_REDIRECTS`].
pub(super) async fn execute(
    spec: Spec,
    mut payload: Payload,
    authorize: &Authorize,
) -> Result<Answer, AlefError> {
    let Spec {
        mut url,
        mut method,
        mut headers,
        follow,
    } = spec;
    if !headers.contains_key(USER_AGENT) {
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("Alef/", env!("CARGO_PKG_VERSION"))),
        );
    }
    let mut redirected = false;
    let mut hops = 0;
    loop {
        authorize(url.as_str())?;
        let uri = Uri::try_from(url.as_str()).map_err(|_| network("the address is not valid"))?;
        let mut request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .body(payload.take())
            .map_err(|_| network("the request is not valid"))?;
        *request.headers_mut() = headers.clone();
        let response = pool().request(request).await.map_err(|error| {
            if error.is_connect() {
                network("cannot connect to the server")
            } else {
                network("the request failed")
            }
        })?;
        let next = if follow {
            next_hop(
                response.status(),
                response.headers().get(LOCATION),
                &url,
                &mut method,
                &mut headers,
                &mut payload,
            )?
        } else {
            None
        };
        let Some(next) = next else {
            return Ok(Answer {
                response,
                url,
                redirected,
            });
        };
        if hops == MAX_REDIRECTS {
            return Err(network("the server redirected too many times"));
        }
        hops += 1;
        url = next;
        redirected = true;
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn carrying() -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("content-type", "text/plain"),
            ("content-encoding", "gzip"),
            ("content-language", "en"),
            ("content-location", "/x"),
            ("authorization", "secret"),
            ("cookie", "a=b"),
            ("x-keep", "1"),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        headers
    }

    /// What a hop makes of a request: the next address, the method, the headers, the payload.
    struct Hop {
        next: Result<Option<Url>, AlefError>,
        method: Method,
        headers: HeaderMap,
        payload: Payload,
    }

    fn hop(status: u16, location: Option<&str>, method: Method, payload: Payload) -> Hop {
        let location = location.map(|text| HeaderValue::from_str(text).unwrap());
        hop_with(status, location, "http://a.test/dir/page", method, payload)
    }

    fn hop_with(
        status: u16,
        location: Option<HeaderValue>,
        from: &str,
        mut method: Method,
        mut payload: Payload,
    ) -> Hop {
        let mut headers = carrying();
        let next = next_hop(
            StatusCode::from_u16(status).unwrap(),
            location.as_ref(),
            &url(from),
            &mut method,
            &mut headers,
            &mut payload,
        );
        Hop {
            next,
            method,
            headers,
            payload,
        }
    }

    fn in_memory() -> Payload {
        Payload::Bytes(Bytes::from_static(b"body"))
    }

    fn goes_to(hop: &Hop, expected: &str) {
        let next = hop.next.as_ref().expect("no error").as_ref();
        assert_eq!(next.map(Url::as_str), Some(expected));
    }

    #[test]
    fn the_origin_is_the_scheme_the_host_and_the_port() {
        assert!(same_origin(
            &url("http://a.test/x"),
            &url("http://a.test:80/y")
        ));
        assert!(same_origin(
            &url("https://a.test/"),
            &url("https://a.test:443/")
        ));
        assert!(!same_origin(
            &url("http://a.test/"),
            &url("https://a.test/")
        ));
        assert!(
            !same_origin(&url("http://a.test:8443/"), &url("https://a.test:8443/")),
            "the same number of the port under another scheme is another origin"
        );
        assert!(!same_origin(&url("http://a.test/"), &url("http://b.test/")));
        assert!(!same_origin(
            &url("http://a.test/"),
            &url("http://a.test:81/")
        ));
    }

    #[test]
    fn a_see_other_is_a_get_for_every_method_and_loses_the_body() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let hop = hop(303, Some("/next"), method.clone(), in_memory());
            goes_to(&hop, "http://a.test/next");
            assert_eq!(hop.method, Method::GET, "{method}");
            assert!(matches!(hop.payload, Payload::Empty));
            for name in [
                "content-type",
                "content-encoding",
                "content-language",
                "content-location",
            ] {
                assert!(!hop.headers.contains_key(name), "{name}");
            }
            assert!(hop.headers.contains_key("x-keep"));
        }
        for method in [Method::GET, Method::HEAD] {
            let hop = hop(303, Some("/next"), method.clone(), Payload::Empty);
            assert_eq!(hop.method, method);
        }
    }

    #[test]
    fn a_moved_or_found_turns_only_a_post_into_a_get() {
        for status in [301, 302] {
            let post = hop(status, Some("/next"), Method::POST, in_memory());
            assert_eq!(post.method, Method::GET, "{status}");
            assert!(matches!(post.payload, Payload::Empty));
            assert!(!post.headers.contains_key("content-type"));

            let streamed = hop(status, Some("/next"), Method::POST, Payload::Stream(None));
            goes_to(&streamed, "http://a.test/next");
            assert!(
                matches!(streamed.payload, Payload::Empty),
                "the body of a stream is dropped with the method"
            );

            let put = hop(status, Some("/next"), Method::PUT, in_memory());
            goes_to(&put, "http://a.test/next");
            assert_eq!(put.method, Method::PUT, "{status}");
            assert!(matches!(put.payload, Payload::Bytes(_)));
            assert!(put.headers.contains_key("content-type"));

            let unsent = hop(status, Some("/next"), Method::PUT, Payload::Stream(None));
            assert!(
                matches!(unsent.next, Ok(None)),
                "a body that went cannot go again"
            );
            let head = hop(status, Some("/next"), Method::HEAD, Payload::Empty);
            assert_eq!(head.method, Method::HEAD);
        }
    }

    #[test]
    fn a_temporary_or_permanent_redirect_keeps_the_method_and_sends_the_body_again() {
        for status in [307, 308] {
            for method in [Method::POST, Method::PUT, Method::DELETE, Method::GET] {
                let hop = hop(status, Some("/next"), method.clone(), in_memory());
                goes_to(&hop, "http://a.test/next");
                assert_eq!(hop.method, method, "{status}");
                assert!(matches!(hop.payload, Payload::Bytes(_)));
                assert!(hop.headers.contains_key("content-type"));
            }
            let unsent = hop(status, Some("/next"), Method::POST, Payload::Stream(None));
            assert!(matches!(unsent.next, Ok(None)), "{status}");
        }
    }

    #[test]
    fn what_is_no_redirect_to_follow_is_the_answer_itself() {
        for status in [300, 304, 305] {
            let hop = hop(status, Some("/next"), Method::GET, Payload::Empty);
            assert!(matches!(hop.next, Ok(None)), "{status}");
        }
        for status in [301, 302, 303, 307, 308] {
            let hop = hop(status, None, Method::GET, Payload::Empty);
            assert!(matches!(hop.next, Ok(None)), "{status}");
        }
    }

    #[test]
    fn the_place_is_taken_from_the_address_it_came_from() {
        let relative = hop_with(
            302,
            Some(HeaderValue::from_static("../up?x=1#f")),
            "http://a.test/dir/page",
            Method::GET,
            Payload::Empty,
        );
        goes_to(&relative, "http://a.test/up?x=1#f");
        let absolute = hop(
            302,
            Some("https://b.test:8443/p"),
            Method::GET,
            Payload::Empty,
        );
        goes_to(&absolute, "https://b.test:8443/p");
        let rooted = hop(302, Some("/root"), Method::GET, Payload::Empty);
        goes_to(&rooted, "http://a.test/root");
    }

    #[test]
    fn a_place_that_is_not_http_or_not_an_address_is_the_network_failing() {
        let bytes = HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap();
        let bad_text = hop_with(
            302,
            Some(bytes),
            "http://a.test/",
            Method::GET,
            Payload::Empty,
        );
        assert_eq!(
            bad_text.next.err().map(|e| e.code),
            Some(ErrorCode::Network)
        );
        for location in [
            "ftp://b.test/",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ws://b.test/",
            "http://[",
        ] {
            let hop = hop(302, Some(location), Method::GET, Payload::Empty);
            assert_eq!(
                hop.next.err().map(|e| e.code),
                Some(ErrorCode::Network),
                "{location}"
            );
        }
    }

    #[test]
    fn the_credentials_stay_in_one_origin_and_stay_behind_when_it_changes() {
        let same = hop(302, Some("/other"), Method::GET, Payload::Empty);
        assert!(same.headers.contains_key("authorization") && same.headers.contains_key("cookie"));
        for place in ["http://b.test/", "https://a.test/", "http://a.test:8080/"] {
            let moved = hop(302, Some(place), Method::GET, Payload::Empty);
            assert!(
                !moved.headers.contains_key("authorization")
                    && !moved.headers.contains_key("cookie"),
                "{place}"
            );
            assert!(moved.headers.contains_key("x-keep"), "{place}");
        }
    }
}
