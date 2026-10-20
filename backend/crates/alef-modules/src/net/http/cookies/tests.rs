// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
struct TestClock {
    mono: Instant,
    wall: SystemTime,
    offset: AtomicU64,
}
impl Clock for TestClock {
    fn mono(&self) -> Instant {
        self.mono + Duration::from_secs(self.offset.load(Ordering::SeqCst))
    }
    fn wall(&self) -> SystemTime {
        self.wall
    }
}
fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}
fn clock() -> (Arc<TestClock>, Jar) {
    let c = Arc::new(TestClock {
        mono: Instant::now(),
        wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        offset: AtomicU64::new(0),
    });
    let j = Jar::with_clock(c.clone());
    (c, j)
}
#[test]
fn path_rules_and_default_path_use_boundaries() {
    assert_eq!(default_path("/a/b"), "/a");
    assert_eq!(default_path("/"), "/");
    assert!(path_match("/a/b", "/a"));
    assert!(!path_match("/ab", "/a"));
}
#[test]
fn domains_are_ignored_secure_is_https_only_and_http_only_is_list_hidden() {
    let (c, j) = clock();
    let https = u("https://example.test/a/b");
    j.store(
        &https,
        ["a=1; Domain=attacker.test; HttpOnly; Secure".to_owned()].into_iter(),
    );
    assert!(j.list(&https).unwrap().is_empty());
    assert_eq!(j.header(&https).as_deref(), Some("a=1"));
    assert!(j.header(&u("http://example.test/a/b")).is_none());
    assert_eq!(j.header(&u("https://attacker.test/a/b")), None);
    let _ = c;
}
#[test]
fn max_age_precedence_and_expired_dates_are_not_session_cookies() {
    let (c, j) = clock();
    let u = u("https://x.test/");
    j.store(
        &u,
        ["a=1; Max-Age=20; Expires=Thu, 01 Jan 1970 00:00:00 GMT".into()].into_iter(),
    );
    assert_eq!(j.header(&u).as_deref(), Some("a=1"));
    j.clear().unwrap();
    j.store(
        &u,
        ["a=1; Max-Age=0; Expires=Wed, 09 Jun 2032 10:18:14 GMT".into()].into_iter(),
    );
    assert_eq!(j.header(&u), None);
    j.store(
        &u,
        ["a=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT".into()].into_iter(),
    );
    assert_eq!(j.header(&u), None);
    j.store(&u, ["a=1; Max-Age=2".into()].into_iter());
    c.offset.store(3, Ordering::SeqCst);
    assert_eq!(j.header(&u), None);
}
#[test]
fn maximum_max_age_falls_back_to_a_bounded_deadline_without_panicking() {
    let (_, j) = clock();
    let url = u("https://x.test/");
    j.store(&url, [format!("a=1; Max-Age={}", i64::MAX)].into_iter());
    assert_eq!(j.header(&url).as_deref(), Some("a=1"));
}

#[test]
fn secure_http_is_dropped_domains_are_host_only_and_expiry_is_monotonic() {
    let (c, j) = clock();
    let url = u("http://co.uk/");
    j.store(
        &url,
        ["secure=x; Secure".into(), "host=x; Domain=co.uk".into()].into_iter(),
    );
    assert_eq!(j.header(&url).as_deref(), Some("host=x"));
    assert_eq!(j.header(&u("https://co.uk/")).as_deref(), Some("host=x"));
    assert!(j.header(&u("http://sub.co.uk/")).is_none());
    j.store(&url, ["host=x; Max-Age=-1".into()].into_iter());
    assert!(j.header(&url).is_none());
    j.store(
        &url,
        ["future=x; Expires=Thu, 01 Jan 2030 00:00:00 GMT".into()].into_iter(),
    );
    assert!(j.header(&url).is_some());
    c.offset.store(400_000_000, Ordering::SeqCst);
    assert!(j.header(&url).is_none());
}
#[test]
fn clone_clear_and_close_cannot_revive_cookies() {
    let (_, j) = clock();
    let u = u("https://x.test/");
    j.store(&u, ["a=b".into()].into_iter());
    let clone = j.clone();
    j.clear().unwrap();
    assert!(clone.header(&u).is_none());
    j.store(&u, ["a=b".into()].into_iter());
    j.close();
    clone.store(&u, ["a=new".into()].into_iter());
    assert!(clone.header(&u).is_none());
    assert!(j.0.state.lock().unwrap().cookies.is_empty());
    assert_eq!(
        clone.list(&u).unwrap_err().code,
        alef_core::ErrorCode::NotFound
    );
}
#[test]
fn controls_invalid_names_and_oversized_lines_are_dropped() {
    let (_, j) = clock();
    let u = u("https://x.test/");
    for s in ["a=bad\n", "bad name=x", "a=bad\u{7f}"] {
        j.store(&u, [s.into()].into_iter());
    }
    j.store(&u, [format!("a={}", "x".repeat(4096))].into_iter());
    assert!(j.header(&u).is_none());
}
#[test]
fn date_values_match_expected_epochs_and_bad_inputs_fail() {
    use std::time::UNIX_EPOCH;
    assert_eq!(date::parse("01 Jan 1970 00:00:00 GMT"), Some(UNIX_EPOCH));
    assert_eq!(
        date::parse("09 Jun 2021 10:18:14 GMT")
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_623_233_894
    );
    assert_eq!(
        date::parse("09 2021 Jun 10:18:14 GMT")
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1_623_233_894
    );
    assert_eq!(
        date::parse("01 Jan 69 00:00:00 GMT")
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        3_124_224_000
    );
    assert!(date::parse("01 Jan 1960 00:00:00 GMT").unwrap() < UNIX_EPOCH);
    assert!(date::parse("31 Feb 2021 10:00:00 GMT").is_none());
}
#[test]
fn line_host_jar_and_header_limits_drop_whole_new_cookies() {
    let (_, j) = clock();
    let base = u("https://h0.test/");
    let accepted = format!("a={}", "x".repeat(4094));
    j.store(&base, [accepted].into_iter());
    assert_eq!(j.header(&base).unwrap().len(), 4096);
    j.store(&base, [format!("b={}", "x".repeat(4095))].into_iter());
    assert!(j.list(&base).unwrap().iter().all(|(n, _)| n != "b"));
    for n in 0..50 {
        let url = u(&format!("https://h0.test/{n}/"));
        j.store(&url, [format!("c{n}=v; Path=/")].into_iter());
    }
    let before = j.list(&base).unwrap().len();
    assert_eq!(before, 50);
    j.store(&base, ["last=v".into()].into_iter());
    assert_eq!(j.list(&base).unwrap().len(), before);
    for h in 1..61 {
        let url = u(&format!("https://h{h}.test/"));
        for n in 0..50 {
            j.store(&url, [format!("c{n}=v")].into_iter());
        }
    }
    let header_jar = Jar::new();
    let hu = u("https://headers.test/");
    for (name, size) in [("a", 2046), ("b", 2046), ("c", 2047), ("d", 2047)] {
        header_jar.store(
            &hu,
            [format!("{name}={}", "x".repeat(size - 2))].into_iter(),
        );
    }
    assert_eq!(header_jar.header(&hu).unwrap().len(), 8192);
    header_jar.store(&hu, [format!("last={}", "x".repeat(10))].into_iter());
    assert!(!header_jar.header(&hu).unwrap().contains("last="));
    let total = j.0.state.lock().unwrap().cookies.len();
    assert_eq!(total, 3000);
    assert!(j.list(&u("https://h60.test/")).unwrap().is_empty());
}
