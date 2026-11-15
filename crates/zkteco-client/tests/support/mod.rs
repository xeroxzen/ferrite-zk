//! A mock ZK device for tests: tokio TCP and UDP servers that speak the
//! protocol with `zkteco-proto`'s own codecs, share one in-memory device
//! (users, attendance, templates), record every request, and inject faults
//! on demand.

#![allow(dead_code, unreachable_pub)]

mod device;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::watch;
use zkteco_client::{ClientConfig, RetryPolicy, Transport};
use zkteco_proto::framing::{encode_tcp, TcpFrameDecoder};
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::{ChecksumPolicy, Command};

#[allow(unused_imports, reason = "each test file uses a different subset")]
pub use device::{event_with, punches, templates, users, Delivery, DeviceState, Faults};

/// A running mock device. Dropping it stops the server.
pub struct MockDevice {
    pub addr: SocketAddr,
    pub udp_addr: SocketAddr,
    state: Arc<Mutex<DeviceState>>,
    /// Bumped whenever the outbox changes; every server task watches it.
    pushed: watch::Sender<u64>,
    server: tokio::task::JoinHandle<()>,
    udp_server: tokio::task::JoinHandle<()>,
}

impl Drop for MockDevice {
    fn drop(&mut self) {
        self.server.abort();
        self.udp_server.abort();
    }
}

/// A TCP listener and a UDP socket on the same port number, as on a real
/// device, so `Transport::Auto` can fall back. The UDP port may already be
/// taken (another test's client socket), so try a few fresh ports.
async fn bind_pair() -> (TcpListener, UdpSocket) {
    for _ in 0..64 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        if let Ok(udp) = UdpSocket::bind(addr).await {
            return (listener, udp);
        }
    }
    panic!("no port free for both TCP and UDP");
}

impl MockDevice {
    pub async fn start(state: DeviceState) -> Self {
        let (listener, udp) = bind_pair().await;
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        let (pushed, watcher) = watch::channel(0);
        let tcp_watcher = watcher.clone();
        let server = tokio::spawn(async move {
            // Bounded by the test's lifetime: the task is aborted on drop.
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                shared.lock().unwrap().connections += 1;
                tokio::spawn(serve(stream, Arc::clone(&shared), tcp_watcher.clone()));
            }
        });
        let udp_addr = udp.local_addr().expect("udp local addr");
        let udp_server = tokio::spawn(serve_udp(udp, Arc::clone(&state), watcher));
        Self {
            addr,
            udp_addr,
            state,
            pushed,
            server,
            udp_server,
        }
    }

    /// A device that answers only over UDP: nothing listens on its TCP
    /// port.
    pub async fn start_udp_only(state: DeviceState) -> Self {
        let state = Arc::new(Mutex::new(state));
        let udp = UdpSocket::bind("127.0.0.1:0").await.expect("bind udp");
        let udp_addr = udp.local_addr().expect("udp local addr");
        let (pushed, watcher) = watch::channel(0);
        let udp_server = tokio::spawn(serve_udp(udp, Arc::clone(&state), watcher));
        Self {
            addr: udp_addr,
            udp_addr,
            state,
            pushed,
            server: tokio::spawn(async {}),
            udp_server,
        }
    }

    /// As [`config`](Self::config), over UDP.
    pub fn config_udp(&self) -> ClientConfig {
        self.config().transport(Transport::Udp).port(self.udp_addr.port())
    }

    /// A client configuration for this device with short timeouts.
    pub fn config(&self) -> ClientConfig {
        ClientConfig::new(self.addr.ip().to_string())
            .port(self.addr.port())
            .transport(Transport::Tcp)
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

    /// Send `n` attendance events to the connected client, unprompted.
    pub fn push_events(&self, n: usize) {
        {
            let mut st = self.state();
            for _ in 0..n {
                let event = st.event_packet();
                st.outbox.push(event);
                // A real device also stores the punch in its log.
                let record = st.last_event_record();
                st.attendance.push(record);
            }
        }
        self.pushed.send_modify(|v| *v += 1);
    }

    /// Send these unframed packets to the connected client, unprompted.
    pub fn push_packets(&self, packets: Vec<Vec<u8>>) {
        self.state().outbox.extend(packets);
        self.pushed.send_modify(|v| *v += 1);
    }

    /// Store `n` punches in the log without sending events, as happens
    /// while no client is connected.
    pub fn record_offline(&self, n: usize) {
        let mut st = self.state();
        for _ in 0..n {
            st.events_sent += 1;
            let record = st.last_event_record();
            st.attendance.push(record);
        }
    }

    /// Close the TCP connection from the device's side.
    pub fn drop_connection(&self) {
        self.state().close_pushed = true;
        self.pushed.send_modify(|v| *v += 1);
    }
}

/// Take the outbox, and whether to close, if there is anything to deliver.
fn take_outbox(state: &Mutex<DeviceState>) -> (Vec<Vec<u8>>, bool) {
    let mut st = state.lock().unwrap();
    (
        std::mem::take(&mut st.outbox),
        std::mem::take(&mut st.close_pushed),
    )
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<DeviceState>>, mut pushed: watch::Receiver<u64>) {
    stream.set_nodelay(true).expect("nodelay");
    let mut frames = TcpFrameDecoder::default();
    let mut buf = vec![0u8; 4096];
    pushed.mark_unchanged();
    // Ends when the client closes the socket or a reply asks to close.
    loop {
        let read = tokio::select! {
            read = stream.read(&mut buf) => read,
            changed = pushed.changed() => {
                if changed.is_err() {
                    return;
                }
                let (packets, close) = take_outbox(&state);
                let delivery = state.lock().unwrap().faults.delivery;
                if write_replies(&mut stream, &packets, delivery).await.is_err() || close {
                    return;
                }
                continue;
            }
        };
        let n = match read {
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

/// Serve datagrams: one request per datagram, one datagram per reply packet.
/// Delivery modes do not apply. Each `CMD_CONNECT` counts as a connection.
async fn serve_udp(socket: UdpSocket, state: Arc<Mutex<DeviceState>>, mut pushed: watch::Receiver<u64>) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut last_peer = None;
    // Ends when the task is aborted or the socket fails.
    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buf) => received,
            changed = pushed.changed() => {
                if changed.is_err() {
                    return;
                }
                // Pushed packets go to the last client heard from; with
                // none, they wait (a TCP connection may take them).
                if let Some(peer) = last_peer {
                    let (packets, _) = take_outbox(&state);
                    for p in &packets {
                        if socket.send_to(p, peer).await.is_err() {
                            return;
                        }
                    }
                }
                continue;
            }
        };
        let Ok((n, peer)) = received else {
            return;
        };
        let raw = bytes::Bytes::copy_from_slice(&buf[..n]);
        let request = packet::decode(raw, ChecksumPolicy::Reject).expect("client checksums are valid");
        // A client that has said goodbye takes no more pushed packets;
        // otherwise this task could steal events meant for a later TCP
        // connection.
        last_peer = (request.header.command != Command::Exit).then_some(peer);
        let outcome = {
            let mut st = state.lock().unwrap();
            if request.header.command == Command::Connect {
                st.connections += 1;
            }
            st.handle(&request)
        };
        for reply in &outcome.replies {
            if socket.send_to(reply, peer).await.is_err() {
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
