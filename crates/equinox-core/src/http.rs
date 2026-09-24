//! Lightweight async HTTP client based on libsoup3, driven by the GLib main loop.

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use gio::prelude::*;
use soup::prelude::*;
use soup::{Message, Session, Status};

pub const USER_AGENT: &str = concat!("Equinox/", env!("CARGO_PKG_VERSION"));

/// HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: Status,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

#[derive(Clone)]
pub struct HttpClient {
    session: Session,
    /// (cancellable, response stream) of the request currently in flight, so
    /// [`abort_inflight`](Self::abort_inflight) can interrupt it immediately.
    /// The daemon is single-flight (one request at a time), so at most one
    /// pair is live.
    inflight: std::cell::RefCell<Option<(gio::Cancellable, Option<gio::InputStream>)>>,
}

impl HttpClient {
    pub fn new(user_agent: &str) -> Self {
        let session = Session::builder()
            // 60 s request timeout: generous for slow image CDNs, but the
            // daemon task queue lets the user cancel updates so a stuck
            // source can't tie up the GUI indefinitely.
            .timeout(60)
            .user_agent(user_agent)
            .build();
        Self {
            session,
            inflight: std::cell::RefCell::new(None),
        }
    }

    /// GET `url`, following redirects. Returns status, body and Content-Type.
    pub async fn get(&self, url: &str) -> Result<HttpResponse> {
        let msg = Message::new("GET", url)
            .with_context(|| format!("{}: {url}", gettext("Invalid URL")))?;
        let canc = gio::Cancellable::new();
        // Register the cancellable BEFORE sending so abort_inflight can stop
        // the request even while it is waiting for response headers.
        *self.inflight.borrow_mut() = Some((canc.clone(), None));
        let stream = send_with_cancel(&self.session, &msg, &canc)
            .await
            .with_context(|| format!("{}: {url}", gettext("Network request failed")))?;
        // Body read in progress → remember the stream so closing it aborts it.
        if let Some((_, slot)) = self.inflight.borrow_mut().as_mut() {
            *slot = Some(stream.clone());
        }
        let status = msg.status();
        let content_type = msg
            .response_headers()
            .and_then(|h| h.content_type())
            .map(|(t, _params)| t.to_string());
        let body = read_stream(stream, &canc).await?;
        *self.inflight.borrow_mut() = None;
        Ok(HttpResponse {
            status,
            body,
            content_type,
        })
    }

    /// GET requiring HTTP 200, returning the body.
    pub async fn get_ok(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.get(url).await?;
        if resp.status != Status::Ok {
            // Diagnostic message; status codes and URLs are not translatable.
            return Err(anyhow!("HTTP {status:?}: {url}", status = resp.status));
        }
        Ok(resp.body)
    }

    /// Abort the request currently in flight (cancel + close its stream), so
    /// a cancelled update stops a hung network fetch immediately instead of
    /// riding out the 60 s request timeout.
    pub fn abort_inflight(&self) {
        if let Some((canc, stream)) = self.inflight.borrow_mut().take() {
            canc.cancel();
            if let Some(s) = stream {
                let _ = s.close(None::<&gio::Cancellable>);
            }
        }
    }
}

/// [`soup::Session::send_async`] driven through the GLib main loop with an
/// explicit cancellable, so the operation can be interrupted externally.
///
/// `glib::MainContext::channel` does not exist (E0599); a oneshot channel
/// bridges soup's callback into an async fn cleanly. The sender side is
/// dropped when the callback runs, so the receiver errors out if the request
/// is aborted before the callback fires.
fn send_with_cancel(
    session: &Session,
    msg: &Message,
    canc: &gio::Cancellable,
) -> impl std::future::Future<Output = Result<gio::InputStream>> {
    let (tx, rx) = futures_channel::oneshot::channel();
    let canc = canc.clone();
    session.send_async(
        msg,
        glib::Priority::DEFAULT,
        Some(&canc),
        move |res| {
            let _ = tx.send(res);
        },
    );
    async move {
        rx.await
            .map_err(|_| anyhow!(gettext("Network request failed")))?
            .map_err(Into::into)
    }
}

async fn read_stream(stream: gio::InputStream, canc: &gio::Cancellable) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        // Cancellation checkpoint between buffers: abort_inflight() takes the
        // (cancellable, stream) pair out of the client, and there is a tiny
        // window between send_async completing and the stream being registered.
        // Checking the cancellable here covers both the stream being closed
        // (read below fails with G_IO_ERROR_CLOSED) and that window.
        if canc.is_cancelled() {
            return Err(anyhow!(gettext("Network request failed")));
        }
        let buf = vec![0u8; 64 * 1024];
        // Newer GLib removed G_IO_ERROR_EOF: reading 0 bytes means end of stream.
        let (buf, n, err) = stream
            .read_all_future(buf, glib::Priority::DEFAULT)
            .await
            .map_err(|(_buf, e)| e)?;
        if let Some(e) = err {
            return Err(e.into());
        }
        out.extend_from_slice(&buf[..n]);
        if n == 0 {
            break;
        }
    }
    Ok(out)
}

/// Infer the image file extension from Content-Type; falls back to the URL's
/// extension when unknown.
pub fn image_extension(content_type: Option<&str>, url: &str) -> String {
    if let Some(ct) = content_type {
        let ct = ct.split(';').next().unwrap_or(ct).trim().to_ascii_lowercase();
        let ext = match ct.as_str() {
            "image/jpeg" | "image/jpg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/avif" => "avif",
            "image/gif" => "gif",
            "image/tiff" => "tiff",
            _ => "",
        };
        if !ext.is_empty() {
            return ext.to_owned();
        }
    }
    // Fallback: take the extension from the URL
    let path = url.split('?').next().unwrap_or(url);
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "webp" | "avif" | "gif" | "tif" | "tiff"))
        .unwrap_or_else(|| "jpg".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{image_extension, HttpClient};
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn extension_from_content_type() {
        assert_eq!(image_extension(Some("image/jpeg"), "https://x/a"), "jpg");
        assert_eq!(image_extension(Some("image/png"), "https://x/a"), "png");
        assert_eq!(image_extension(Some("image/webp; charset=binary"), "https://x/a"), "webp");
        assert_eq!(image_extension(Some("image/tiff"), "https://x/a"), "tiff");
    }

    #[test]
    fn extension_fallback_to_url() {
        assert_eq!(image_extension(None, "https://x/a.avif"), "avif");
        assert_eq!(image_extension(None, "https://x/a.tif"), "tif");
        assert_eq!(image_extension(None, "https://x/photo?w=1920"), "jpg");
        assert_eq!(image_extension(Some("text/html"), "https://x/photo"), "jpg");
    }

    /// A local HTTP server that sends the headers fast but dribbles the body
    /// out slowly, so a reader is caught mid-body when the test aborts it.
    fn slow_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 5000000\r\n\r\n",
                )
                .expect("write headers");
            let chunk = [0u8; 64 * 1024];
            for _ in 0..80 {
                if stream.write_all(&chunk).is_err() {
                    break; // client aborted the connection
                }
                stream.flush().ok();
                thread::sleep(Duration::from_millis(50));
            }
        });
        (format!("http://{addr}/slow"), handle)
    }

    /// Regression: cancel must interrupt a HUNG body read immediately instead
    /// of riding out the 60 s request timeout. This exercises the exact daemon
    /// path (`abort_inflight` that cancels the cancellable + closes the
    /// stream) mid-download on the glib main loop.
    ///
    /// NB: the client is shared through `Rc` because `HttpClient` is
    /// `#[derive(Clone)]` (a shallow clone would hand the aborter its OWN
    /// `RefCell`, i.e. an empty inflight slot — the same trap as cloning the
    /// daemon's `state.http` instead of borrowing it).
    #[test]
    fn abort_interrupts_slow_body_read() {
        let (url, server) = slow_server();
        let client = std::rc::Rc::new(HttpClient::new("equinox-test"));
        let ctx = glib::MainContext::default();

        // Fire the abort 300 ms into the download (headers arrive fast, the
        // body is still streaming). Runs on the same main loop that block_on
        // drives below — same thread as the daemon's cancel path.
        let aborter = std::rc::Rc::clone(&client);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            aborter.abort_inflight();
        });

        let started = std::time::Instant::now();
        let res = ctx.block_on(client.get(&url));
        let elapsed = started.elapsed();
        server.join().ok();

        // The abort must surface as an immediate error — well under the 60 s
        // soup timeout, and under 5 s even on a loaded CI box.
        assert!(res.is_err(), "aborted get() must return an error, got {res:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "abort took {elapsed:?}, expected the fetch to be interrupted immediately"
        );
    }
}
