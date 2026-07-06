//! The Bluetooth side. Runs on its own thread with a current-thread tokio
//! runtime. Talks to BlueZ over D-Bus via `bluer`.
//!
//! Responsibilities:
//!  * register a "just works" pairing agent (no PIN prompts for audio/mice/pads)
//!  * keep discovery running for the lifetime of the app so new devices appear
//!  * refresh the shared snapshot on a timer and after every event/command
//!  * execute Connect/Disconnect/Remove commands (spawned so a slow connect
//!    never blocks the event loop)

use crate::model::{Command, DeviceView, Shared, Snapshot};
use bluer::agent::Agent;
use bluer::{Adapter, Address, Session};
use futures::{pin_mut, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// Entry point called from the spawned std::thread (see main.rs).
pub fn thread_main(shared: Shared, cmd_rx: UnboundedReceiver<Command>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            set_status(&shared, format!("failed to start async runtime: {e}"));
            return;
        }
    };

    if let Err(e) = rt.block_on(run(shared.clone(), cmd_rx)) {
        set_status(&shared, format!("bluetooth error: {e}"));
    }
}

async fn run(shared: Shared, mut cmd_rx: UnboundedReceiver<Command>) -> bluer::Result<()> {
    let session = Session::new().await?;

    // "Just works" agent: with no passkey/pin callbacks BlueZ uses the
    // NoInputNoOutput flow, and we auto-accept confirmation/authorization so
    // headsets, mice and controllers pair without any prompt.
    // Param types are annotated so closure/return-type inference through the
    // boxed dyn Fn is unambiguous.
    let agent = Agent {
        request_default: true,
        request_confirmation: Some(Box::new(|_req: bluer::agent::RequestConfirmation| {
            Box::pin(async move { Ok(()) })
        })),
        request_authorization: Some(Box::new(|_req: bluer::agent::RequestAuthorization| {
            Box::pin(async move { Ok(()) })
        })),
        ..Default::default()
    };
    // Keep the handle alive for the whole session; dropping it unregisters.
    let _agent_handle = session.register_agent(agent).await?;

    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
    set_status(&shared, format!("using adapter {}", adapter.name()));

    // Discovery stays on for the app's (short) lifetime so new devices appear.
    let discover = adapter.discover_devices().await?;
    pin_mut!(discover);

    let mut refresh = tokio::time::interval(Duration::from_millis(1200));

    // Initial paint so the list isn't empty on the first frame.
    publish(&adapter, &shared).await;

    loop {
        tokio::select! {
            // A discovery event just means "something changed" — repaint below.
            // Match `Some(_)` so that if the stream ever ends we don't spin on a
            // perpetually-ready `None`; the other branches keep the loop alive.
            Some(_) = discover.next() => {}

            maybe_cmd = cmd_rx.recv() => {
                match maybe_cmd {
                    Some(cmd) => handle_command(&adapter, &shared, cmd),
                    None => break, // UI thread dropped the sender -> app exiting
                }
            }

            _ = refresh.tick() => {}
        }

        publish(&adapter, &shared).await;
    }

    Ok(())
}

/// Dispatch a UI command. Connect/Disconnect/Remove can each take seconds, so
/// we spawn them; the select loop stays responsive and keeps repainting.
fn handle_command(adapter: &Adapter, shared: &Shared, cmd: Command) {
    let adapter = adapter.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        let result = match &cmd {
            Command::Connect(addr) => connect_smart(&adapter, *addr).await,
            Command::Disconnect(addr) => disconnect(&adapter, *addr).await,
            Command::Remove(addr) => remove(&adapter, *addr).await,
        };
        match result {
            Ok(msg) => set_status(&shared, msg),
            Err(e) => set_status(&shared, format!("{cmd:?} failed: {e}")),
        }
        publish(&adapter, &shared).await;
    });
}

/// Pair (if needed) + trust + connect. This is the "A button" action.
async fn connect_smart(adapter: &Adapter, addr: Address) -> bluer::Result<String> {
    let device = adapter.device(addr)?;
    if !device.is_paired().await? {
        device.pair().await?;
    }
    // Trusting lets the device reconnect on its own next time.
    let _ = device.set_trusted(true).await;
    device.connect().await?;
    Ok(format!("connected {}", label(&device.name().await.ok().flatten(), addr)))
}

async fn disconnect(adapter: &Adapter, addr: Address) -> bluer::Result<String> {
    let device = adapter.device(addr)?;
    let name = device.name().await.ok().flatten();
    device.disconnect().await?;
    Ok(format!("disconnected {}", label(&name, addr)))
}

async fn remove(adapter: &Adapter, addr: Address) -> bluer::Result<String> {
    // remove_device unpairs and forgets the device.
    adapter.remove_device(addr).await?;
    Ok(format!("removed {addr}"))
}

/// Enumerate all known devices and publish a fresh snapshot for the UI.
async fn publish(adapter: &Adapter, shared: &Shared) {
    let mut devices = Vec::new();

    if let Ok(addrs) = adapter.device_addresses().await {
        for addr in addrs {
            let Ok(dev) = adapter.device(addr) else { continue };
            let name = dev
                .name()
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| addr.to_string());
            devices.push(DeviceView {
                addr,
                name,
                icon: dev.icon().await.ok().flatten(),
                paired: dev.is_paired().await.unwrap_or(false),
                connected: dev.is_connected().await.unwrap_or(false),
                trusted: dev.is_trusted().await.unwrap_or(false),
                rssi: dev.rssi().await.ok().flatten(),
            });
        }
    }

    // Connected first, then paired, then strongest signal, then by name.
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(b.rssi.unwrap_or(-127).cmp(&a.rssi.unwrap_or(-127)))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let powered = adapter.is_powered().await.unwrap_or(false);

    if let Ok(mut s) = shared.lock() {
        // Preserve the last status line across refreshes.
        let status = std::mem::take(&mut s.status);
        *s = Snapshot {
            devices,
            scanning: true,
            powered,
            status,
        };
    }
}

fn set_status(shared: &Shared, msg: String) {
    if let Ok(mut s) = shared.lock() {
        s.status = msg;
    }
}

fn label(name: &Option<String>, addr: Address) -> String {
    name.clone().unwrap_or_else(|| addr.to_string())
}
