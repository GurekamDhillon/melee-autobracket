//! A stub bracket server for tests that need routes or answers the real server does not
//! give: it records every request and answers each from a handler.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use proto::{Next, Role};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the stub received.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub path: String,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

type Handler = Arc<dyn Fn(&Req) -> (u16, String) + Send + Sync>;

pub struct Stub {
    pub url: String,
    log: Arc<Mutex<Vec<Req>>>,
}

impl Stub {
    /// Serves on a free loopback port; `handler` gives each request's status and JSON body.
    pub async fn start(handler: impl Fn(&Req) -> (u16, String) + Send + Sync + 'static) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let shared = log.clone();
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                tokio::spawn(serve(conn, handler.clone(), shared.clone()));
            }
        });
        Stub { url, log }
    }

    /// The requests received so far whose method is `method` and whose path ends with `suffix`.
    pub fn requests(&self, method: &str, suffix: &str) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path.ends_with(suffix))
            .cloned()
            .collect()
    }

    /// How many requests have a path starting with `prefix`.
    pub fn count(&self, prefix: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.starts_with(prefix))
            .count()
    }

    /// The path of every request so far, in the order they arrived.
    pub fn paths(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }

    /// How many `GET /v1/me/next` polls came after the first request whose path ends with
    /// `suffix` (0 if there was none yet).
    pub fn polls_after(&self, suffix: &str) -> usize {
        let log = self.log.lock().unwrap();
        let Some(i) = log.iter().position(|r| r.path.ends_with(suffix)) else {
            return 0;
        };
        log[i + 1..]
            .iter()
            .filter(|r| r.path.starts_with("/v1/me/next"))
            .count()
    }
}

async fn serve(mut conn: TcpStream, handler: Handler, log: Arc<Mutex<Vec<Req>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        // One request per connection: headers, then `content-length` bytes of body.
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match conn.read(&mut chunk).await {
                Ok(n @ 1..) => buf.extend_from_slice(&chunk[..n]),
                _ => return,
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or("").split(' ');
        let method = first.next().unwrap_or("").to_string();
        let path = first.next().unwrap_or("").to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            match conn.read(&mut chunk).await {
                Ok(n @ 1..) => buf.extend_from_slice(&chunk[..n]),
                _ => return,
            }
        }
        let body = buf[head_end..head_end + len].to_vec();
        buf.drain(..head_end + len);
        let req = Req {
            method,
            path,
            headers,
            body,
        };
        let (status, json) = handler(&req);
        log.lock().unwrap().push(req);
        let res = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{json}",
            json.len()
        );
        if conn.write_all(res.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// `Next::Play` for set 0, game 1, this player hosting.
pub fn host_play() -> Next {
    Next::Play {
        set_id: 0,
        opponent_tag: "Bob".into(),
        role: Role::Host,
        relay: "relay.example:7000".into(),
        room_code: None,
        game_no: 1,
        wins: [0, 0],
        best_of: 3,
        stocks: 4,
        timer_secs: 480,
        awaiting_verdict: false,
    }
}

pub fn json(next: &Next) -> String {
    serde_json::to_string(next).unwrap()
}
