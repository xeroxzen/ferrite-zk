//! Merge live events from several devices into one stream, each device
//! reconnecting and backfilling on its own.
//!
//! One task per device owns its `LiveCapture` and sends events into a
//! shared channel; a watch channel tells every task to stop.
//!
//! ```sh
//! ZK_HOSTS=10.0.0.5,10.0.0.6 cargo run --example multi_device_live
//! ```

use std::future::Future;

use tokio::sync::{mpsc, watch};
use zkteco_client::{CaptureOptions, CapturedEvent, ClientConfig, LiveCapture, ReconnectPolicy};

/// Events from every device, tagged with its name, until `shutdown`.
/// Returns once every device has unsubscribed.
pub async fn run(
    devices: Vec<(String, ClientConfig)>,
    shutdown: impl Future<Output = ()>,
    events: mpsc::Sender<(String, CapturedEvent)>,
) {
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    for (name, config) in devices {
        tasks.spawn(device(name, config, events.clone(), stop_rx.clone()));
    }
    drop(events);
    shutdown.await;
    stop_tx.send_replace(true);
    while let Some(done) = tasks.join_next().await {
        if let Err(e) = done {
            eprintln!("device task failed: {e}");
        }
    }
}

async fn device(
    name: String,
    config: ClientConfig,
    events: mpsc::Sender<(String, CapturedEvent)>,
    mut stop: watch::Receiver<bool>,
) {
    let opts = CaptureOptions {
        reconnect: Some(ReconnectPolicy::default()),
        backfill: true,
        ..CaptureOptions::default()
    };
    let mut capture = match LiveCapture::connect(config, opts).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{name}: {e}");
            return;
        }
    };
    // Ends when told to stop, when the stream gives up, or when nobody is
    // listening any more.
    loop {
        tokio::select! {
            item = capture.next() => match item {
                Some(Ok(event)) => {
                    if events.send((name.clone(), event)).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => eprintln!("{name}: {e}"),
                None => break,
            },
            _ = stop.changed() => break,
        }
    }
    let (client, result) = capture.stop().await;
    if let Err(e) = result {
        eprintln!("{name}: unsubscribing: {e}");
    }
    if let Err(e) = client.disconnect().await {
        eprintln!("{name}: disconnecting: {e}");
    }
}

#[tokio::main]
async fn main() {
    let hosts = std::env::var("ZK_HOSTS").unwrap_or_else(|_| "192.168.1.201".into());
    let devices = hosts
        .split(',')
        .map(|h| (h.to_owned(), ClientConfig::new(h)))
        .collect();
    let (tx, mut rx) = mpsc::channel::<(String, CapturedEvent)>(1024);
    let printer = tokio::spawn(async move {
        while let Some((name, event)) = rx.recv().await {
            println!("{name}: {:?} {:?}", event.source, event.event);
        }
    });
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("cannot listen for Ctrl-C: {e}");
        }
    };
    run(devices, ctrl_c, tx).await;
    if let Err(e) = printer.await {
        eprintln!("printer failed: {e}");
    }
}
