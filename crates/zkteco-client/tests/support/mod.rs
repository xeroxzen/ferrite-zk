//! A mock ZK device for tests: a tokio TCP server that speaks the protocol
//! with `zkteco-proto`'s own codecs, keeps users, attendance and templates
//! in memory, records every request, and injects faults on demand.

#![allow(dead_code, unreachable_pub)]

mod device;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use zkteco_client::{ClientConfig, RetryPolicy};
use zkteco_proto::framing::{encode_tcp, TcpFrameDecoder};
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::{ChecksumPolicy, Command};

pub use device::{punches, templates, users, Delivery, DeviceState, Faults};

/// A running mock device. Dropping it stops the server.
pub struct MockDevice {
    pub addr: SocketAddr,
    state: Arc<Mutex<DeviceState>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for MockDevice {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl MockDevice {
    pub async fn start(state: DeviceState) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let server = tokio::spawn(async move {
            // Bounded by the test's lifetime: the task is aborted on drop.
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                shared.lock().unwrap().connections += 1;
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        Self { addr, state, server }
    }

    /// A client configuration for this device with short timeouts.
    pub fn config(&self) -> ClientConfig {
        ClientConfig::new(self.addr.ip().to_string())
            .port(self.addr.port())
            .connect_timeout(Duration::from_secs(2))
            .request_timeout(Duration::from_millis(500))
            .retry(RetryPolicy {
                attempts: 3,
                backoff: Duration::from_millis(5),
            })
    }

    pub fn state(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().unwrap()
    }

    /// Commands received, in order.
    pub fn commands(&self) -> Vec<Command> {
        self.state().log.iter().map(|p| p.header.command).collect()
    }

    pub fn count(&self, command: Command) -> usize {
        self.commands().iter().filter(|c| **c == command).count()
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<DeviceState>>) {
    stream.set_nodelay(true).expect("nodelay");
    let mut frames = TcpFrameDecoder::default();
    let mut buf = vec![0u8; 4096];
    // Ends when the client closes the socket or a reply asks to close.
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        frames.extend(&buf[..n]);
        while let Some(raw) = frames.next_packet().expect("client frames are valid") {
            let request = packet::decode(raw, ChecksumPolicy::Reject).expect("client checksums are valid");
            let outcome = state.lock().unwrap().handle(&request);
            let delivery = state.lock().unwrap().faults.delivery;
            if write_replies(&mut stream, &outcome.replies, delivery)
                .await
                .is_err()
                || outcome.close
            {
                return;
            }
        }
    }
}

/// Write reply packets (unframed) according to the delivery mode.
async fn write_replies(
    stream: &mut TcpStream,
    replies: &[Vec<u8>],
    delivery: Delivery,
) -> std::io::Result<()> {
    let frames: Vec<Vec<u8>> = replies.iter().map(|p| encode_tcp(p).to_vec()).collect();
    match delivery {
        Delivery::Separate => {
            for f in &frames {
                stream.write_all(f).await?;
                stream.flush().await?;
            }
        }
        Delivery::Coalesced => {
            stream.write_all(&frames.concat()).await?;
            stream.flush().await?;
        }
        Delivery::Split(piece) => {
            for part in frames.concat().chunks(piece.max(1)) {
                stream.write_all(part).await?;
                stream.flush().await?;
                tokio::task::yield_now().await;
            }
        }
    }
    Ok(())
}

/// Decode a packet the mock produced, for assertions.
pub fn decode(bytes: &[u8]) -> Packet {
    packet::decode(bytes::Bytes::copy_from_slice(bytes), ChecksumPolicy::Reject).unwrap()
}
