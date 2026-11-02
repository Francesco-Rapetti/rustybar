//! The network, in-process: WebSockets and HTTP requests through the system's URLSession, with its
//! TLS, the certificates trusted in the keychain, proxies and VPNs; and HTTP over a Unix socket, for
//! the Docker Engine.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse, NSURLSession,
    NSURLSessionWebSocketCloseCode, NSURLSessionWebSocketMessage, NSURLSessionWebSocketTask,
};

/// A WebSocket connection, closed when dropped.
pub struct Socket(Retained<NSURLSessionWebSocketTask>);

impl Socket {
    /// Connects to `url` (`ws://` or `wss://`). `on` gets each message, then None once when the
    /// connection is gone, on a thread of URLSession.
    pub fn open(url: &str, on: impl Fn(Option<String>) + Send + Sync + 'static) -> Option<Socket> {
        let url = NSURL::URLWithString(&NSString::from_str(url))?;
        let task = NSURLSession::sharedSession().webSocketTaskWithURL(&url);
        // The first message of Home Assistant has every entity it follows
        task.setMaximumMessageSize(64 << 20);
        task.resume();
        receive(task.clone(), Arc::new(on));
        Some(Socket(task))
    }

    pub fn send(&self, text: &str) {
        let message = NSURLSessionWebSocketMessage::initWithString(NSURLSessionWebSocketMessage::alloc(), &NSString::from_str(text));
        // A send that fails fails the receive too, which reports it
        let done = RcBlock::new(|_: *mut NSError| {});
        unsafe { self.0.sendMessage_completionHandler(&message, &done) };
    }

    /// `pong` gets whether the other end answered.
    pub fn ping(&self, pong: impl Fn(bool) + Send + Sync + 'static) {
        let done = RcBlock::new(move |error: *mut NSError| pong(error.is_null()));
        unsafe { self.0.sendPingWithPongReceiveHandler(&done) };
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.0.cancelWithCloseCode_reason(NSURLSessionWebSocketCloseCode::GoingAway, None);
    }
}

/// One message at a time: each one asks for the next.
fn receive(task: Retained<NSURLSessionWebSocketTask>, on: Arc<dyn Fn(Option<String>) + Send + Sync>) {
    let next = task.clone();
    let done = RcBlock::new(move |message: *mut NSURLSessionWebSocketMessage, error: *mut NSError| {
        match unsafe { message.as_ref() } {
            Some(m) if error.is_null() => {
                let text = m.string().map(|s| s.to_string()).or_else(|| m.data().map(|d| String::from_utf8_lossy(&d.to_vec()).into_owned()));
                on(Some(text.unwrap_or_default()));
                receive(next.clone(), on.clone());
            }
            _ => on(None),
        }
    });
    unsafe { task.receiveMessageWithCompletionHandler(&done) };
}

/// An HTTP request. `done` gets the status, 0 when the server can't be reached, and the body, on a
/// thread of URLSession.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: f64, done: impl Fn(u16, Vec<u8>) + Send + Sync + 'static) {
    let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) else { return done(0, Vec::new()) };
    let request = NSMutableURLRequest::requestWithURL(&u);
    request.setHTTPMethod(&NSString::from_str(method));
    request.setTimeoutInterval(timeout);
    for (name, value) in headers {
        request.setValue_forHTTPHeaderField(Some(&NSString::from_str(value)), &NSString::from_str(name));
    }
    if let Some(body) = body {
        request.setHTTPBody(Some(&NSData::with_bytes(body)));
    }
    let handler = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, _: *mut NSError| {
        let status = unsafe { response.as_ref() }.and_then(|r| r.downcast_ref::<NSHTTPURLResponse>()).map(|r| r.statusCode() as u16).unwrap_or(0);
        let body = unsafe { data.as_ref() }.map(|d| d.to_vec()).unwrap_or_default();
        done(status, body);
    });
    let task = unsafe { NSURLSession::sharedSession().dataTaskWithRequest_completionHandler(&request, &handler) };
    task.resume();
}

/// An HTTP response read from a Unix socket: the body is unchunked as it's read.
pub struct Response {
    pub status: u16,
    pub body: Box<dyn BufRead + Send>,
    /// The connection, to close it from another thread (a streaming body never ends by itself).
    pub stream: UnixStream,
}

impl Response {
    pub fn text(mut self) -> io::Result<String> {
        let mut s = String::new();
        self.body.read_to_string(&mut s)?;
        Ok(s)
    }
}

/// `GET target` over the Unix socket at `path`. Blocking: for worker threads. `timeout` bounds
/// each read, None for a stream that waits for events.
pub fn unix_get(path: &Path, target: &str, timeout: Option<Duration>) -> io::Result<Response> {
    unix_request(path, "GET", target, timeout)
}

/// `method target` without a body (`POST`, `DELETE`…) over the Unix socket at `path`, as
/// `unix_get`.
pub fn unix_request(path: &Path, method: &str, target: &str, timeout: Option<Duration>) -> io::Result<Response> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // A POST without a length could be waiting for its body
    let length = if method == "GET" { "" } else { "Content-Length: 0\r\n" };
    write!(stream, "{method} {target} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: rustybar\r\n{length}Connection: close\r\n\r\n")?;
    let closer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| io::Error::other("not HTTP"))?;
    let (mut chunked, mut length) = (false, None);
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<u64>().ok();
        }
    }
    let body: Box<dyn BufRead + Send> = match (chunked, length) {
        (true, _) => Box::new(BufReader::new(Chunked { inner: reader, left: 0, done: false })),
        (false, Some(n)) => Box::new(BufReader::new(reader.take(n))),
        (false, None) => Box::new(reader),
    };
    Ok(Response { status, body, stream: closer })
}

/// A chunked body, without the chunk sizes.
struct Chunked<R: BufRead> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // The size line, after the CRLF that ends the previous chunk
            let mut line = String::new();
            while line.trim().is_empty() {
                line.clear();
                if self.inner.read_line(&mut line)? == 0 {
                    self.done = true;
                    return Ok(0);
                }
            }
            let size = line.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16).map_err(|_| io::Error::other("bad chunk size"))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let n = buf.len().min(self.left);
        let read = self.inner.read(&mut buf[..n])?;
        if read == 0 {
            self.done = true;
        }
        self.left -= read;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked() {
        let body = b"4\r\nWiki\r\n6\r\npedia \r\nE\r\nin \r\n\r\nchunks.\r\n0\r\n\r\n";
        let mut r = Chunked { inner: &body[..], left: 0, done: false };
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "Wikipedia in \r\n\r\nchunks.");
    }
}
