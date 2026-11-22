//! HTTP implementation of [`SonarApi`].
//!
//! - Discovers the base URL lazily and caches it.
//! - On a transport error, drops the cache, rediscovers and retries **once**
//!   (the port changes every GG launch).
//! - Caches the mode for [`MODE_TTL`]. When Sonar answers "Cannot be called in
//!   current mode", refreshes the mode and returns [`SonarError::WrongMode`]
//!   so the caller can re-target (a write meant for one mode can't simply be
//!   replayed in the other).

use std::time::{Duration, Instant};

use ureq::Agent;

use super::discovery::{self, DiscoveryError, excerpt};
use super::{
    AudioDevice, Channel, Mode, Routing, SonarApi, SonarError, SonarResult, StreamOutput, Target,
    Volumes, models, round_volume, routes,
};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
pub const MODE_TTL: Duration = Duration::from_secs(2);

/// Plain-HTTP agent for Sonar's web server on loopback.
///
/// - No proxy (env proxies must never see loopback traffic).
/// - No redirects (a redirect could point off-host).
/// - HTTP error statuses are returned as responses so callers can log the body.
pub fn loopback_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Put,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
        }
    }
}

/// Returns Sonar's base URL (`http://127.0.0.1:PORT`).
pub type Discover = Box<dyn FnMut() -> Result<String, DiscoveryError> + Send>;

pub struct HttpSonar {
    agent: Agent,
    discover: Discover,
    base: Option<String>,
    mode_cache: Option<(Mode, Instant)>,
}

impl HttpSonar {
    /// Client using real discovery (coreProps.json -> GG /subApps).
    pub fn new() -> Self {
        Self::with_discovery(Box::new(discovery::discover))
    }

    pub fn with_discovery(discover: Discover) -> Self {
        Self {
            agent: loopback_agent(REQUEST_TIMEOUT),
            discover,
            base: None,
            mode_cache: None,
        }
    }

    /// Cached base URL, discovering if needed.
    pub fn base_url(&mut self) -> SonarResult<String> {
        if let Some(b) = &self.base {
            return Ok(b.clone());
        }
        let b = (self.discover)()?;
        self.base = Some(b.clone());
        Ok(b)
    }

    /// One request with retry-once-after-rediscovery on transport errors.
    /// Returns status and body without interpreting either.
    pub fn request_raw(&mut self, method: Method, path: &str) -> SonarResult<(u16, String)> {
        let base = self.base_url()?;
        match self.send(method, &base, path) {
            Err(SonarError::Transport { message, .. }) => {
                tracing::info!(
                    "Sonar unreachable at {base} ({message}); rediscovering and retrying once"
                );
                self.base = None;
                self.mode_cache = None;
                let base = self.base_url()?;
                self.send(method, &base, path)
            }
            other => other,
        }
    }

    fn send(&self, method: Method, base: &str, path: &str) -> SonarResult<(u16, String)> {
        let url = format!("{base}{path}");
        tracing::debug!("{} {url}", method.as_str());
        let result = match method {
            Method::Get => self.agent.get(&url).call(),
            Method::Put => self.agent.put(&url).send_empty(),
        };
        let transport = |e: ureq::Error| SonarError::Transport {
            method: method.as_str(),
            url: url.clone(),
            message: e.to_string(),
        };
        let mut resp = result.map_err(transport)?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().map_err(transport)?;
        tracing::debug!(status, bytes = body.len(), "{} {url}", method.as_str());
        Ok((status, body))
    }

    /// Request expecting 2xx. Detects the wrong-mode error.
    fn request(&mut self, method: Method, path: &str) -> SonarResult<String> {
        let (status, body) = self.request_raw(method, path)?;
        let url = format!("{}{path}", self.base.as_deref().unwrap_or(""));
        if body.contains(routes::WRONG_MODE_TEXT) {
            self.mode_cache = None;
            let current = self.mode()?;
            tracing::info!("{url}: Sonar says wrong mode; now {}", current.label());
            return Err(SonarError::WrongMode { url, current });
        }
        if !(200..300).contains(&status) {
            let err = SonarError::Status {
                method: method.as_str(),
                url,
                status,
                excerpt: excerpt(&body),
            };
            tracing::warn!("{err}");
            return Err(err);
        }
        Ok(body)
    }

    fn get_parsed<T>(
        &mut self,
        path: &str,
        parse: impl FnOnce(&str) -> models::ParseResult<T>,
    ) -> SonarResult<T> {
        let body = self.request(Method::Get, path)?;
        parse(&body).map_err(|message| SonarError::Parse {
            url: format!("{}{path}", self.base.as_deref().unwrap_or("")),
            message,
            excerpt: excerpt(&body),
        })
    }
}

impl Default for HttpSonar {
    fn default() -> Self {
        Self::new()
    }
}

impl SonarApi for HttpSonar {
    fn mode(&mut self) -> SonarResult<Mode> {
        if let Some((mode, at)) = self.mode_cache
            && at.elapsed() < MODE_TTL
        {
            return Ok(mode);
        }
        let mode = self.get_parsed(routes::MODE, models::parse_mode)?;
        if let Some((old, _)) = self.mode_cache
            && old != mode
        {
            tracing::info!("Sonar mode changed: {} -> {}", old.label(), mode.label());
        }
        self.mode_cache = Some((mode, Instant::now()));
        Ok(mode)
    }

    fn invalidate_mode(&mut self) {
        self.mode_cache = None;
    }

    fn set_mode(&mut self, mode: Mode) -> SonarResult<()> {
        self.request(Method::Put, &routes::set_mode(mode))?;
        self.mode_cache = None;
        tracing::info!("set Sonar mode to {}", mode.label());
        Ok(())
    }

    fn volumes(&mut self, mode: Mode) -> SonarResult<Volumes> {
        self.get_parsed(routes::volume_settings(mode), |b| {
            models::parse_volumes(b, mode)
        })
    }

    fn set_volume(&mut self, target: Target, volume: f64) -> SonarResult<()> {
        let v = round_volume(volume)?;
        self.request(Method::Put, &routes::set_volume(target, v))?;
        Ok(())
    }

    fn set_mute(&mut self, target: Target, muted: bool) -> SonarResult<()> {
        self.request(Method::Put, &routes::set_mute(target, muted))?;
        Ok(())
    }

    fn devices(&mut self) -> SonarResult<Vec<AudioDevice>> {
        self.get_parsed(routes::AUDIO_DEVICES, models::parse_devices)
    }

    fn routing(&mut self) -> SonarResult<Routing> {
        let classic =
            self.get_parsed(routes::CLASSIC_REDIRECTIONS, models::parse_classic_routing)?;
        let stream = self.get_parsed(routes::STREAM_REDIRECTIONS, models::parse_stream_routing)?;
        Ok(Routing { classic, stream })
    }

    fn set_classic_device(&mut self, channel: Channel, device_id: &str) -> SonarResult<()> {
        let path = routes::set_classic_device(channel, device_id).ok_or(
            SonarError::Unsupported("master has no output device of its own"),
        )?;
        self.request(Method::Put, &path)?;
        tracing::info!("routed {channel} to {device_id}");
        Ok(())
    }

    fn set_stream_device(&mut self, output: StreamOutput, device_id: &str) -> SonarResult<()> {
        let path = routes::set_stream_device(output, device_id).ok_or(SonarError::Unsupported(
            "switching devices in Streamer mode isn't supported yet (route unknown, see Q4)",
        ))?;
        self.request(Method::Put, &path)?;
        tracing::info!("routed {} to {device_id}", output.name());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sonar::Mix;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    /// Minimal HTTP/1.1 server: answers `"METHOD /path"` from `routes`
    /// (404 otherwise) and logs every request line.
    fn fake_sonar(routes: Vec<(&str, u16, &str)>) -> (String, Log) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: HashMap<String, (u16, String)> = routes
            .into_iter()
            .map(|(k, s, b)| (k.to_owned(), (s, b.to_owned())))
            .collect();
        let log: Log = Arc::default();
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                // Drain headers.
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let key: String = line
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                log2.lock().unwrap().push(key.clone());
                let (status, body) = routes.get(&key).cloned().unwrap_or((404, String::new()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, log)
    }

    fn dead_base() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        base
    }

    fn client_for(bases: Vec<String>) -> (HttpSonar, Arc<Mutex<usize>>) {
        let calls = Arc::new(Mutex::new(0usize));
        let c2 = calls.clone();
        let client = HttpSonar::with_discovery(Box::new(move || {
            let mut n = c2.lock().unwrap();
            let b = bases[(*n).min(bases.len() - 1)].clone();
            *n += 1;
            Ok(b)
        }));
        (client, calls)
    }

    #[test]
    fn rediscovers_and_retries_once_after_transport_error() {
        let (live, log) = fake_sonar(vec![("GET /mode/", 200, "\"classic\"")]);
        let (mut c, calls) = client_for(vec![dead_base(), live]);
        assert_eq!(c.mode().unwrap(), Mode::Classic);
        assert_eq!(*calls.lock().unwrap(), 2);
        assert_eq!(log.lock().unwrap().as_slice(), ["GET /mode/"]);
    }

    #[test]
    fn gives_up_after_one_retry() {
        let (mut c, calls) = client_for(vec![dead_base(), dead_base()]);
        assert!(matches!(c.mode(), Err(SonarError::Transport { .. })));
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[test]
    fn mode_is_cached() {
        let (live, log) = fake_sonar(vec![("GET /mode/", 200, "\"stream\"")]);
        let (mut c, _) = client_for(vec![live]);
        assert_eq!(c.mode().unwrap(), Mode::Stream);
        assert_eq!(c.mode().unwrap(), Mode::Stream);
        assert_eq!(log.lock().unwrap().len(), 1);
        c.invalidate_mode();
        c.mode().unwrap();
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn wrong_mode_refreshes_and_reports_current_mode() {
        let (live, log) = fake_sonar(vec![
            (
                "PUT /volumeSettings/classic/game/Volume/0.42",
                500,
                "Cannot be called in current mode",
            ),
            ("GET /mode/", 200, "\"stream\""),
        ]);
        let (mut c, _) = client_for(vec![live]);
        let err = c
            .set_volume(Target::Classic(Channel::Game), 0.4249)
            .unwrap_err();
        assert!(
            matches!(
                err,
                SonarError::WrongMode {
                    current: Mode::Stream,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["PUT /volumeSettings/classic/game/Volume/0.42", "GET /mode/"]
        );
    }

    #[test]
    fn http_error_carries_status_and_body() {
        let (live, _) = fake_sonar(vec![(
            "PUT /volumeSettings/streamer/monitoring/aux/volume/0.50",
            400,
            "bad slider",
        )]);
        let (mut c, _) = client_for(vec![live]);
        let err = c
            .set_volume(Target::Stream(Mix::Monitoring, Channel::Aux), 0.5)
            .unwrap_err();
        match err {
            SonarError::Status {
                status, excerpt, ..
            } => {
                assert_eq!(status, 400);
                assert_eq!(excerpt, "bad slider");
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn invalid_volume_never_hits_the_network() {
        let (live, log) = fake_sonar(vec![]);
        let (mut c, _) = client_for(vec![live]);
        assert!(matches!(
            c.set_volume(Target::Classic(Channel::Game), f64::NAN),
            Err(SonarError::InvalidVolume(_))
        ));
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn parse_error_includes_excerpt() {
        let (live, _) = fake_sonar(vec![("GET /audioDevices", 200, "{\"oops\":1}")]);
        let (mut c, _) = client_for(vec![live]);
        match c.devices().unwrap_err() {
            SonarError::Parse { excerpt, .. } => assert_eq!(excerpt, "{\"oops\":1}"),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn stream_device_switch_is_unsupported_for_now() {
        let (mut c, calls) = client_for(vec![dead_base()]);
        assert!(matches!(
            c.set_stream_device(StreamOutput::Mic, "x"),
            Err(SonarError::Unsupported(_))
        ));
        assert_eq!(*calls.lock().unwrap(), 0);
    }
}
