// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session-local RFC6265 subset. Domain is ignored; cookies remain host-only. Resource limit is the
//! existing per-session limit (default 1024), so jars have no process-global registry.
use super::invalid;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use url::Url;
const LINE: usize = 4096;
pub(super) const LINE_LIMIT: usize = LINE;
const HOST: usize = 50;
const JAR: usize = 3000;
const HEADER: usize = 8192;
trait Clock: Send + Sync {
    fn mono(&self) -> Instant;
    fn wall(&self) -> SystemTime;
}
struct SystemClock;
impl Clock for SystemClock {
    fn mono(&self) -> Instant {
        Instant::now()
    }
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}
#[derive(Clone)]
pub(super) struct Jar(Arc<Inner>);
struct Inner {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
}
struct State {
    closed: bool,
    cookies: Vec<Cookie>,
    next: u64,
}
#[derive(Clone)]
struct Cookie {
    name: String,
    value: String,
    host: String,
    path: String,
    secure: bool,
    http_only: bool,
    deadline: Option<Instant>,
    created: u64,
}
impl Jar {
    pub(super) fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }
    fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                closed: false,
                cookies: Vec::new(),
                next: 0,
            }),
            clock,
        }))
    }
    pub(super) fn close(&self) {
        let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        s.cookies.clear();
    }
    pub(super) fn clear(&self) -> Result<(), alef_core::AlefError> {
        let mut s = self.live()?;
        s.cookies.clear();
        Ok(())
    }
    fn live(&self) -> Result<std::sync::MutexGuard<'_, State>, alef_core::AlefError> {
        let s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.closed {
            Err(alef_core::AlefError::new(
                alef_core::ErrorCode::NotFound,
                "cookie jar is closed",
            ))
        } else {
            Ok(s)
        }
    }
    pub(super) fn store<I: Iterator<Item = String>>(&self, url: &Url, values: I) {
        for v in values {
            if v.len() > LINE {
                continue;
            }
            let now = self.0.clock.mono();
            let wall = self.0.clock.wall();
            let Ok(mut s) = self.live() else { return };
            purge(&mut s, now);
            let Some(c) = parse(&v, url, now, wall, s.next) else {
                continue;
            };
            s.next = s.next.saturating_add(1);
            let old = s
                .cookies
                .iter()
                .position(|x| x.name == c.name && x.host == c.host && x.path == c.path);
            let created = old.map(|i| s.cookies[i].created);
            if let Some(i) = old {
                s.cookies.remove(i);
            }
            let mut c = c;
            if let Some(created) = created {
                c.created = created;
            }
            if c.deadline.is_some_and(|d| d <= now) {
                continue;
            }
            let host_count = s.cookies.iter().filter(|x| x.host == c.host).count();
            if host_count >= HOST || s.cookies.len() >= JAR {
                continue;
            }
            s.cookies.push(c);
        }
    }
    pub(super) fn header(&self, url: &Url) -> Option<String> {
        let now = self.0.clock.mono();
        let mut s = self.live().ok()?;
        purge(&mut s, now);
        select(&s, url)
    }
    pub(super) fn list(&self, url: &Url) -> Result<Vec<(String, String)>, alef_core::AlefError> {
        let now = self.0.clock.mono();
        let mut s = self.live()?;
        purge(&mut s, now);
        let host = url
            .host_str()
            .ok_or_else(|| invalid("cookie URL has no host"))?
            .to_ascii_lowercase();
        let all = selected(&s, url);
        let mut used = 0;
        let mut out = Vec::new();
        for c in all {
            let n = c.name.len() + 1 + c.value.len() + if used == 0 { 0 } else { 2 };
            if used + n > HEADER {
                continue;
            }
            used += n;
            if !c.http_only && c.host == host {
                out.push((c.name.clone(), c.value.clone()))
            }
        }
        Ok(out)
    }
}
fn purge(s: &mut State, now: Instant) {
    s.cookies.retain(|c| c.deadline.is_none_or(|d| d > now));
}
fn select(s: &State, u: &Url) -> Option<String> {
    let mut out = String::new();
    for x in selected(s, u) {
        let add = x.name.len() + 1 + x.value.len() + if out.is_empty() { 0 } else { 2 };
        if out.len() + add > HEADER {
            continue;
        }
        if !out.is_empty() {
            out.push_str("; ")
        }
        out.push_str(&x.name);
        out.push('=');
        out.push_str(&x.value);
    }
    (!out.is_empty()).then_some(out)
}
fn selected<'a>(s: &'a State, u: &Url) -> Vec<&'a Cookie> {
    let Some(host) = u.host_str().map(str::to_ascii_lowercase) else {
        return Vec::new();
    };
    let mut c: Vec<_> = s
        .cookies
        .iter()
        .filter(|c| {
            c.host == host && path_match(u.path(), &c.path) && (!c.secure || u.scheme() == "https")
        })
        .collect();
    c.sort_by_key(|c| (std::cmp::Reverse(c.path.len()), c.created));
    c
}
fn path_match(p: &str, c: &str) -> bool {
    p == c || (p.starts_with(c) && (c.ends_with('/') || p.as_bytes().get(c.len()) == Some(&b'/')))
}
fn default_path(p: &str) -> String {
    if !p.starts_with('/') || p == "/" {
        return "/".into();
    }
    p.rfind('/')
        .filter(|i| *i > 0)
        .map_or("/".into(), |i| p[..i].into())
}
fn parse(line: &str, u: &Url, now: Instant, wall: SystemTime, created: u64) -> Option<Cookie> {
    if line.len() > LINE || line.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let mut p = line.split(';');
    let (name, value) = p.next()?.trim().split_once('=')?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        || value
            .bytes()
            .any(|b| !matches!(b, 0x21..=0x7e) || b == b'\\')
    {
        return None;
    }
    let mut path = None;
    let (mut secure, mut httponly) = (false, false);
    let (mut age, mut expires) = (None, None);
    for a in p {
        let (k, v) = a.trim().split_once('=').unwrap_or((a.trim(), ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "domain" => {}
            "path" if v.starts_with('/') => path = Some(v.to_owned()),
            "secure" => secure = true,
            "httponly" => httponly = true,
            "max-age" => age = v.trim().parse::<i64>().ok(),
            "expires" => expires = date::parse(v.trim()),
            _ => {}
        }
    }
    if secure && u.scheme() != "https" {
        return None;
    }
    let deadline = if let Some(a) = age {
        if a <= 0 {
            Some(now)
        } else {
            now.checked_add(Duration::from_secs(a as u64))
                .or_else(|| now.checked_add(Duration::from_secs(400 * 86400)))
        }
    } else {
        expires.map(|t| match t.duration_since(wall) {
            Ok(d) => now
                .checked_add(d)
                .or_else(|| now.checked_add(Duration::from_secs(400 * 86400)))
                .unwrap_or(now),
            Err(_) => now,
        })
    };
    Some(Cookie {
        name: name.into(),
        value: value.into(),
        host: u.host_str()?.to_ascii_lowercase(),
        path: path.unwrap_or_else(|| default_path(u.path())),
        secure,
        http_only: httponly,
        deadline,
        created,
    })
}
pub(super) mod commands;
pub(super) mod date;
pub(super) use commands::register as register_commands;
#[cfg(test)]
mod tests;
