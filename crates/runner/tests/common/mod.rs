#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use metro_runner::config::Config;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn new(status: u16, body: &str) -> Self {
        Resp {
            status,
            headers: vec![],
            body: body.into(),
        }
    }
}

/// Minimal single-threaded HTTP/1.1 server; one request per connection.
pub struct MockServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    pub fn start(handler: impl Fn(&Req, usize) -> Resp + Send + 'static) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs = requests.clone();
        std::thread::spawn(move || {
            for (n, stream) in l.incoming().enumerate() {
                let Ok(mut s) = stream else { continue };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut headers = HashMap::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                r.read_exact(&mut body).unwrap();
                let req = Req {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into(),
                };
                let resp = handler(&req, n);
                reqs.lock().unwrap().push(req);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                    resp.status,
                    resp.body.len()
                );
                if !resp.body.is_empty() {
                    out.push_str("Content-Type: application/json\r\n");
                }
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&resp.body);
                let _ = s.write_all(out.as_bytes());
            }
        });
        MockServer { url, requests }
    }

    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn config(ws: &Path, extra: &[(&str, &str)]) -> Config {
    let mut m: HashMap<String, String> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    m.entry("METRO_RUNNER_TOKEN".into())
        .or_insert_with(|| "test-token".into());
    m.insert("METRO_WORKSPACE".into(), ws.to_string_lossy().into_owned());
    Config::from_lookup(|k| m.get(k).cloned()).expect("valid config")
}
