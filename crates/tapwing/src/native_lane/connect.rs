//! The TCP connector under the driver's gRPC channel.
//!
//! It exists to answer one question after an input RPC failed: did the
//! request reach the device? Behind `adb forward` a dead driver does not look
//! like a refused connection. The adb server accepts the local connection,
//! finds nothing listening on the device and closes it. An HTTP/2 client
//! writes its preface and its first request as soon as the TCP connection is
//! up, so by then the request is in a socket buffer and "it was never read"
//! is something only the peer knows.
//!
//! [`ServerFirstConnector`] closes that gap. An HTTP/2 server sends its
//! `SETTINGS` frame without waiting for the client (grpc-okhttp queues it
//! before it starts reading), so after the TCP connect the connector waits
//! for the peer's first byte before it hands the stream to hyper. A peer that
//! closes or resets the connection before that byte gets
//! [`ClosedBeforeServerSpoke`]: the host has not written a single byte, which
//! proves the request was not delivered. A peer that stays silent for
//! [`SERVER_FIRST_GRACE`] gets the stream handed over as before, and every
//! later failure on it is ambiguous again.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tonic::transport::Uri;
use tower_service::Service;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// How long a new connection waits for the server's first byte. A live driver
/// answers within one adb round trip and a dead forward is closed as fast, so
/// this only runs out against a peer that is neither; then the connection is
/// used without the proof.
pub(super) const SERVER_FIRST_GRACE: Duration = Duration::from_millis(500);

/// The peer accepted the connection and then closed or reset it before it
/// sent a byte, while this host had not written one.
#[derive(Debug)]
pub(super) struct ClosedBeforeServerSpoke(Option<std::io::Error>);

impl fmt::Display for ClosedBeforeServerSpoke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the connection was closed before the driver sent anything (nothing was sent to it)")
    }
}

impl std::error::Error for ClosedBeforeServerSpoke {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.as_ref().map(|e| e as _)
    }
}

/// Connects over TCP, then lets the server speak first (see the module docs).
#[derive(Clone)]
pub(super) struct ServerFirstConnector {
    http: HttpConnector,
    grace: Duration,
}

impl ServerFirstConnector {
    pub(super) fn new(grace: Duration) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_nodelay(true);
        Self { http, grace }
    }
}

impl Service<Uri> for ServerFirstConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connecting = self.http.call(uri);
        let grace = self.grace;
        Box::pin(async move {
            let io = connecting.await?;
            wait_for_server(io.inner(), grace).await?;
            Ok(io)
        })
    }
}

/// Waits until `stream` has a byte to read (left in place for hyper), the
/// peer ends the connection, or `grace` passes.
async fn wait_for_server(stream: &TcpStream, grace: Duration) -> Result<(), ClosedBeforeServerSpoke> {
    let mut first = [0u8; 1];
    match tokio::time::timeout(grace, stream.peek(&mut first)).await {
        Ok(Ok(0)) => Err(ClosedBeforeServerSpoke(None)),
        Ok(Err(e)) => Err(ClosedBeforeServerSpoke(Some(e))),
        // The server spoke, or it is silent so far: either way the stream is usable.
        Ok(Ok(_)) | Err(_) => Ok(()),
    }
}

/// Loopback peers for the delivery tests: each binds once, serves from its own
/// thread and tells the test what it saw over a channel.
#[cfg(test)]
pub(crate) mod testing {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    /// An empty HTTP/2 `SETTINGS` frame: the server's connection preface.
    const SETTINGS: [u8; 9] = [0, 0, 0, 4, 0, 0, 0, 0, 0];
    const CLIENT_PREFACE_LEN: usize = 24;
    const FRAME_DATA: u8 = 0;
    const FLAG_END_STREAM: u8 = 1;

    /// What a peer does with a connection it accepted.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Peer {
        /// Closes it at once: `adb forward` whose device end is gone.
        Closes,
        /// Resets it at once (`RST` instead of `FIN`).
        Resets,
        /// Sends its `SETTINGS`, reads one whole request, then resets without answering.
        ReadsRequestThenResets,
        /// Sends nothing, reads one whole request once the client gives up waiting, then resets.
        SilentReadsRequestThenResets,
    }

    /// Closes `stream` with a `RST`.
    fn reset(stream: TcpStream) {
        let _ = socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO));
        drop(stream);
    }

    /// Reads the client preface and frames up to the end of the first request
    /// (a `DATA` frame with `END_STREAM`). `false` when the client hung up first.
    fn read_one_request(stream: &mut TcpStream) -> bool {
        let mut preface = [0u8; CLIENT_PREFACE_LEN];
        if stream.read_exact(&mut preface).is_err() {
            return false;
        }
        loop {
            let mut header = [0u8; 9];
            if stream.read_exact(&mut header).is_err() {
                return false;
            }
            let len = usize::from(header[0]) << 16 | usize::from(header[1]) << 8 | usize::from(header[2]);
            let mut payload = vec![0u8; len];
            if stream.read_exact(&mut payload).is_err() {
                return false;
            }
            if header[3] == FRAME_DATA && header[4] & FLAG_END_STREAM != 0 {
                return true;
            }
        }
    }

    /// Serves every connection to a fresh loopback port as `peer` says.
    /// Returns the port and a channel that gets one message per connection:
    /// whether a whole request was read from it.
    pub(crate) fn serve(peer: Peer) -> (u16, Receiver<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let (seen_tx, seen_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let read_request = match peer {
                    Peer::Closes => {
                        drop(stream);
                        false
                    }
                    Peer::Resets => {
                        reset(stream);
                        false
                    }
                    Peer::ReadsRequestThenResets | Peer::SilentReadsRequestThenResets => {
                        if peer == Peer::ReadsRequestThenResets {
                            let _ = stream.write_all(&SETTINGS);
                        }
                        let read = read_one_request(&mut stream);
                        reset(stream);
                        read
                    }
                };
                if seen_tx.send(read_request).is_err() {
                    break;
                }
            }
        });
        (port, seen_rx)
    }
}
