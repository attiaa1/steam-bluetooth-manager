# btpair

A small controller-navigable Bluetooth manager for Steam Big Picture on Linux.

Add it as a non-Steam game, open it with a gamepad, pair or connect your
Bluetooth device, and exit back to Big Picture. It renders its own screen with
SDL2 and talks to BlueZ over D-Bus. It does not hook into or inject anything
into Steam, so a Steam client update can't break it the way it breaks Decky
plugins.

## Controls

- D-pad up/down: move selection
- A: pair (if needed), trust, then connect the selected device
- Y: disconnect (device stays paired)
- Back/Select: remove/unpair the device
- B or Start: exit
- Keyboard fallback for desktop testing: arrows, Enter, D, Delete, Esc

Once a device is paired and connected it is handed off to BlueZ and works
system-wide. This tool only handles the pairing/connection lifecycle and then
gets out of the way.

## Requirements

Linux with BlueZ (any modern distro). Package names below are for the
Debian/Ubuntu family; adjust for your distro.

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Build/link dependencies
sudo apt update
sudo apt install -y build-essential pkg-config \
    libsdl2-dev libsdl2-ttf-dev libdbus-1-dev

# Bluetooth service
sudo systemctl enable --now bluetooth
```

## Build and run

```bash
cargo build --release
./target/release/btpair
```

## Adding it to Steam

1. Library, then Add a Non-Steam Game, and browse to
   `target/release/btpair`.
2. Give it a name and artwork.
3. Launch it from Big Picture and navigate with the gamepad.

Steam passes controller input to launched apps automatically, so navigation
works without extra setup. If the pad doesn't move the selection, set the
shortcut's controller layout to the Gamepad template.

The navigation controller is read through SDL2's GameController API, so it
doesn't matter how it connects: USB, Bluetooth, or a wireless dongle (e.g. the
xone driver for Xbox dongles) all work, as do non-Xbox pads.

## Permissions

Connecting and disconnecting as the active session user is normally permitted.
If pairing or removal fails with an authorization error, add a polkit rule at
`/etc/polkit-1/rules.d/51-btpair.rules`:

```javascript
polkit.addRule(function(action, subject) {
    if (action.id.indexOf("org.bluez.") === 0 && subject.local && subject.active) {
        return polkit.Result.YES;
    }
});
```

## Notes

- Font lookup currently checks a few common paths and honors `BTPAIR_FONT` for
  an override. Resolving via fontconfig (`fc-match`) is the distro-agnostic fix
  and is noted in the source.
- Discovery runs the whole time the app is open, which is fine for a short
  pairing session.
- Pairing uses a just-works agent that auto-confirms. This covers audio
  devices, mice and controllers. Keyboards that require passkey entry are not
  handled yet.
- Removing a device is immediate; there is no confirmation prompt.
