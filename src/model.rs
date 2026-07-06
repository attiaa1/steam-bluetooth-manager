//! Shared data passed between the UI thread and the Bluetooth thread.
//!
//! - The Bluetooth thread OWNS the truth and publishes a `Snapshot` into the
//!   shared `Mutex` ~once a second.
//! - The UI thread READS that snapshot each frame and sends `Command`s back
//!   over an unbounded channel. No blocking Bluetooth work happens on the UI
//!   thread, so the render loop never stalls.

use bluer::Address;
use std::sync::{Arc, Mutex};

/// One row in the device list, as the UI needs to draw it.
#[derive(Clone, Debug)]
pub struct DeviceView {
    pub addr: Address,
    pub name: String,
    /// BlueZ icon hint, e.g. "audio-headset", "input-mouse", "input-gaming".
    pub icon: Option<String>,
    pub paired: bool,
    pub connected: bool,
    pub trusted: bool,
    pub rssi: Option<i16>,
}

impl DeviceView {
    /// Short human label for the device category, derived from the BlueZ icon.
    pub fn kind(&self) -> &'static str {
        match self.icon.as_deref() {
            Some(i) if i.contains("headset") || i.contains("audio") => "audio",
            Some(i) if i.contains("mouse") => "mouse",
            Some(i) if i.contains("keyboard") => "keyboard",
            Some(i) if i.contains("gaming") || i.contains("joystick") => "controller",
            Some(i) if i.contains("phone") => "phone",
            _ => "device",
        }
    }
}

/// Everything the UI needs to render one frame. Replaced wholesale each refresh.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub devices: Vec<DeviceView>,
    pub scanning: bool,
    pub powered: bool,
    /// Last action result / error, shown at the bottom of the screen.
    pub status: String,
}

/// Handle the UI thread holds: shared snapshot + command sender.
pub type Shared = Arc<Mutex<Snapshot>>;

/// Actions the UI asks the Bluetooth thread to perform.
#[derive(Debug, Clone)]
pub enum Command {
    /// Pair (if needed), trust, then connect the given device.
    Connect(Address),
    /// Disconnect the given device (stays paired).
    Disconnect(Address),
    /// Remove/unpair the device entirely.
    Remove(Address),
}
