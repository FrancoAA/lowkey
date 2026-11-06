//! The meetings' screen videos for the page's player, over HTTP on 127.0.0.1.
//!
//! WebKitGTK plays media only from http(s) and file URLs: through a custom scheme (Tauri's asset
//! protocol) the player fails with "source not supported", and the page may not read file URLs.
//! So a small server: loopback only, a random port, a random 128-bit key in every path, GET and
//! HEAD of `m-<id>.webm|mp4` in the meetings folder only, one byte range per request (the
//! player seeks with ranges). Started when the first video is played.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct Server {
    port: u16,
    key: String,
}

impl Server {
    /// Serve the videos of `dir` (on its own threads, as long as the app runs).
    pub fn start(dir: PathBuf) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random numbers from the system: {e}"))?;
        let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let k2 = key.clone();
        std::thread::Builder::new().name("video-server".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let (dir, key) = (dir.clone(), k2.clone());
                let _ = std::thread::Builder::new().name("video-request".into()).spawn(move || {
                    let _ = serve(stream, &dir, &key);
                });
            }
        })?;
        Ok(Self { port, key })
    }

    /// The address of video `file` (a name in the folder).
    pub fn url(&self, file: &str) -> String {
        format!("http://127.0.0.1:{}/{}/{}", self.port, self.key, file)
    }
}

/// The app's server for the meetings folder (started at the first call).
pub fn url(file: &str) -> anyhow::Result<String> {
    static SERVER: OnceLock<Server> = OnceLock::new();
    static STARTING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _one = STARTING.lock();
    if SERVER.get().is_none() {
        let _ = SERVER.set(Server::start(crate::meeting::dir())?);
    }
    Ok(SERVER.get().expect("set above").url(file))
}

/// A video's name: "m-<digits>.webm" or ".mp4", nothing else (no path).
fn video_name(name: &str) -> Option<&'static str> {
    let (id, ext) = name.rsplit_once('.')?;
    if !crate::meeting::valid_id(id) {
        return None;
    }
    match ext {
        "webm" => Some("video/webm"),
        "mp4" => Some("video/mp4"),
        _ => None,
    }
}

/// "bytes=a-b", "bytes=a-", "bytes=-n" of a file of `size` bytes → [from, to]. None: not one
/// range this file has (416).
fn range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (from, to) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size - 1)
        }
        (a, "") => (a.parse().ok()?, size - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(size - 1)),
    };
    (from <= to && from < size).then_some((from, to))
}

fn serve(stream: TcpStream, dir: &Path, key: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut range_header = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range_header = Some(value.trim().to_string());
            }
        }
    }
    let mut out = stream;
    let mut words = first.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let reply = |out: &mut TcpStream, status: &str| write!(out, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    if method != "GET" && method != "HEAD" {
        return reply(&mut out, "405 Method Not Allowed");
    }
    // "/<key>/<name>": the key, then a video's name.
    let found = target.strip_prefix('/').and_then(|t| t.split_once('/')).filter(|(k, _)| *k == key).and_then(|(_, n)| Some((n, video_name(n)?)));
    let Some((name, mime)) = found else { return reply(&mut out, "404 Not Found") };
    let Ok(mut file) = std::fs::File::open(dir.join(name)) else { return reply(&mut out, "404 Not Found") };
    let size = file.metadata()?.len();
    let (status, from, to) = match &range_header {
        None => ("200 OK", 0, size.saturating_sub(1)),
        Some(h) => match range(h, size) {
            Some((a, b)) => ("206 Partial Content", a, b),
            None => {
                return write!(out, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        },
    };
    let length = if size == 0 { 0 } else { to - from + 1 };
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {length}\r\nCache-Control: no-store\r\nConnection: close\r\n");
    if status.starts_with("206") {
        head += &format!("Content-Range: bytes {from}-{to}/{size}\r\n");
    }
    out.write_all(format!("{head}\r\n").as_bytes())?;
    if method == "HEAD" {
        return Ok(());
    }
    file.seek(SeekFrom::Start(from))?;
    // A seek closes the old request: a write error here is the player moving on.
    std::io::copy(&mut file.take(length), &mut out)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, path: &str, range: Option<&str>) -> (String, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let r = range.map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
        write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{r}\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&all[..at]).into_owned(), all[at + 4..].to_vec())
    }

    #[test]
    fn ranges_are_read_as_the_player_sends_them() {
        assert_eq!(range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(range("bytes=90-500", 100), Some((90, 99)));
        assert_eq!(range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(range("bytes=100-", 100), None);
        assert_eq!(range("bytes=0-1,5-6", 100), None);
        assert_eq!(range("items=0-1", 100), None);
    }

    #[test]
    fn only_the_meetings_videos_are_served_with_the_key() {
        let dir = std::env::temp_dir().join(format!("ow-video-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(dir.join("m-12.webm"), &data).unwrap();
        std::fs::write(dir.join("m-12.json"), b"{}").unwrap();
        let s = Server::start(dir.clone()).unwrap();
        let path = s.url("m-12.webm").split_once(&format!(":{}", s.port)).unwrap().1.to_string();

        let (head, body) = get(s.port, &path, None);
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("Content-Type: video/webm") && head.contains("Accept-Ranges: bytes"), "{head}");
        assert_eq!(body, data);
        let (head, body) = get(s.port, &path, Some("bytes=100-199"));
        assert!(head.starts_with("HTTP/1.1 206") && head.contains("Content-Range: bytes 100-199/1000"), "{head}");
        assert_eq!(body, &data[100..200]);
        assert!(get(s.port, &path, Some("bytes=5000-")).0.starts_with("HTTP/1.1 416"));
        // Without the key, another file, or a path out of the folder: nothing.
        for bad in ["/m-12.webm".to_string(), format!("/{}/m-12.json", s.key), format!("/{}/../m-12.webm", s.key), format!("/{}/m-13.webm", s.key), "/x/m-12.webm".into()] {
            assert!(get(s.port, &bad, None).0.starts_with("HTTP/1.1 404"), "{bad}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
