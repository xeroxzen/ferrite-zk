//! Stream live events until Ctrl-C, then unsubscribe cleanly.
//!
//! `LiveCapture::next` is cancel-safe, so it can race a shutdown signal in
//! `tokio::select!` without losing an event.
//!
//! ```sh
//! ZK_HOST=192.168.1.201 cargo run --example live_with_shutdown
//! ```

use std::future::Future;

use zkteco_client::{CaptureOptions, CapturedEvent, ClientConfig, LiveCapture, ReconnectPolicy};

/// Pass each event to `on_event` until `shutdown` completes or the stream
/// ends; then unsubscribe. Events that arrived during the unsubscribe are
/// passed on too.
///
/// # Errors
///
/// A failure to connect, subscribe or unsubscribe.
pub async fn run(
    config: ClientConfig,
    shutdown: impl Future<Output = ()>,
    mut on_event: impl FnMut(&CapturedEvent),
) -> zkteco_client::Result<()> {
    let opts = CaptureOptions::default()
        .reconnect(ReconnectPolicy::default())
        .backfill(true);
    let mut capture = LiveCapture::connect(config, opts).await?;
    let mut shutdown = std::pin::pin!(shutdown);
    // Ends on shutdown or when the stream gives up.
    loop {
        tokio::select! {
            item = capture.next() => match item {
                Some(Ok(event)) => on_event(&event),
                Some(Err(e)) => eprintln!("warning: {e}"),
                None => break,
            },
            () = &mut shutdown => break,
        }
    }
    let (mut client, result) = capture.stop().await;
    for event in client.take_pending_events() {
        eprintln!("arrived while stopping: {event:?}");
    }
    result?;
    client.disconnect().await
}

#[tokio::main]
async fn main() -> zkteco_client::Result<()> {
    let host = std::env::var("ZK_HOST").unwrap_or_else(|_| "192.168.1.201".into());
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("cannot listen for Ctrl-C: {e}");
        }
    };
    run(ClientConfig::new(host), ctrl_c, |e| {
        println!("{:?} {:?}", e.source, e.event);
    })
    .await
}
