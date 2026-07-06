//! btpair — a controller-navigable Bluetooth pairing popup for Steam Big Picture.
//!
//! Architecture: the UI runs on the main thread (SDL2 window + 2D canvas + a
//! TTF font). A second thread runs tokio + bluer and owns all Bluetooth state
//! (see bt.rs). They share a `Snapshot` behind a Mutex; the UI sends `Command`s
//! over an unbounded channel. Nothing here touches Steam's internals, so a
//! Steam update cannot break it.

mod bt;
mod model;

use model::{Command, Shared, Snapshot};

use sdl2::controller::Button;
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use sdl2::render::{Canvas, TextureCreator};
use sdl2::ttf::Font;
use sdl2::video::{Window, WindowContext};

use std::sync::{Arc, Mutex};

const BG: Color = Color::RGB(18, 18, 22);
const PANEL: Color = Color::RGB(30, 30, 38);
const SELECT: Color = Color::RGB(52, 88, 140);
const TEXT: Color = Color::RGB(235, 235, 240);
const DIM: Color = Color::RGB(150, 150, 160);
const GREEN: Color = Color::RGB(120, 210, 130);

fn main() {
    // --- shared state + command channel between UI and BT threads ---
    let shared: Shared = Arc::new(Mutex::new(Snapshot::default()));
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();

    {
        let shared = shared.clone();
        std::thread::spawn(move || bt::thread_main(shared, cmd_rx));
    }

    // --- SDL init ---
    let sdl = sdl2::init().expect("sdl init");
    let video = sdl.video().expect("sdl video");
    let ttf = sdl2::ttf::init().expect("sdl ttf init");
    let controllers_sub = sdl.game_controller().expect("game controller subsystem");

    let window = video
        .window("Bluetooth", 1280, 720)
        .position_centered()
        .fullscreen_desktop()
        .build()
        .expect("create window");

    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .expect("create canvas");
    let texture_creator = canvas.texture_creator();

    let font_path = find_font();
    let font = ttf.load_font(&font_path, 26).expect("load font");
    let font_small = ttf.load_font(&font_path, 20).expect("load small font");

    let mut event_pump = sdl.event_pump().expect("event pump");

    // Keep opened controllers alive (dropping them closes the device).
    let mut controllers = Vec::new();
    for i in 0..controllers_sub.num_joysticks().unwrap_or(0) {
        if controllers_sub.is_game_controller(i) {
            if let Ok(c) = controllers_sub.open(i) {
                controllers.push(c);
            }
        }
    }

    let mut selected: usize = 0;

    'running: loop {
        // --- input ---
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => break 'running,

                Event::ControllerDeviceAdded { which, .. } => {
                    if let Ok(c) = controllers_sub.open(which) {
                        controllers.push(c);
                    }
                }

                Event::ControllerButtonDown { button, .. } => match button {
                    Button::DPadUp => selected = selected.saturating_sub(1),
                    Button::DPadDown => selected += 1,
                    Button::A => send_selected(&cmd_tx, &shared, selected, Action::Connect),
                    Button::Y => send_selected(&cmd_tx, &shared, selected, Action::Disconnect),
                    Button::Back => send_selected(&cmd_tx, &shared, selected, Action::Remove),
                    Button::B | Button::Start => break 'running,
                    _ => {}
                },

                // Keyboard fallback so you can test on a desktop without a pad.
                Event::KeyDown { keycode: Some(k), .. } => match k {
                    Keycode::Up => selected = selected.saturating_sub(1),
                    Keycode::Down => selected += 1,
                    Keycode::Return => send_selected(&cmd_tx, &shared, selected, Action::Connect),
                    Keycode::D => send_selected(&cmd_tx, &shared, selected, Action::Disconnect),
                    Keycode::Delete => send_selected(&cmd_tx, &shared, selected, Action::Remove),
                    Keycode::Escape => break 'running,
                    _ => {}
                },

                _ => {}
            }
        }

        // --- render ---
        let snap = shared.lock().unwrap().clone();
        if !snap.devices.is_empty() {
            selected = selected.min(snap.devices.len() - 1);
        } else {
            selected = 0;
        }

        canvas.set_draw_color(BG);
        canvas.clear();

        let (w, _h) = canvas.output_size().unwrap_or((1280, 720));
        draw_text(&mut canvas, &texture_creator, &font, "Bluetooth", 48, 36, TEXT);
        let header = if snap.powered {
            format!("scanning{}", if snap.scanning { "…" } else { "" })
        } else {
            "adapter OFF".to_string()
        };
        draw_text(&mut canvas, &texture_creator, &font_small, &header, 48, 74, DIM);

        // Device rows.
        let row_h = 64i32;
        let list_top = 120i32;
        for (i, d) in snap.devices.iter().enumerate() {
            let y = list_top + i as i32 * row_h;

            if i == selected {
                canvas.set_draw_color(SELECT);
            } else {
                canvas.set_draw_color(PANEL);
            }
            let _ = canvas.fill_rect(Rect::new(40, y, w - 80, (row_h - 8) as u32));

            let name_color = if d.connected { GREEN } else { TEXT };
            draw_text(&mut canvas, &texture_creator, &font, &d.name, 60, y + 6, name_color);

            let mut tags = Vec::new();
            if d.connected {
                tags.push("connected".to_string());
            } else if d.paired {
                tags.push("paired".to_string());
            }
            tags.push(d.kind().to_string());
            if let Some(r) = d.rssi {
                tags.push(format!("{r} dBm"));
            }
            draw_text(
                &mut canvas,
                &texture_creator,
                &font_small,
                &tags.join("  •  "),
                60,
                y + 36,
                DIM,
            );
        }

        if snap.devices.is_empty() {
            draw_text(
                &mut canvas,
                &texture_creator,
                &font_small,
                "No devices yet — put your device in pairing mode…",
                60,
                list_top + 8,
                DIM,
            );
        }

        // Status line + control hints at the bottom.
        let (_w, h) = canvas.output_size().unwrap_or((1280, 720));
        if !snap.status.is_empty() {
            draw_text(
                &mut canvas,
                &texture_creator,
                &font_small,
                &snap.status,
                48,
                h as i32 - 76,
                DIM,
            );
        }
        draw_text(
            &mut canvas,
            &texture_creator,
            &font_small,
            "A connect/pair   Y disconnect   Back remove   B exit",
            48,
            h as i32 - 44,
            TEXT,
        );

        canvas.present();
    }
    // Dropping cmd_tx closes the channel, which ends the BT thread's loop.
}

enum Action {
    Connect,
    Disconnect,
    Remove,
}

fn send_selected(
    tx: &tokio::sync::mpsc::UnboundedSender<Command>,
    shared: &Shared,
    selected: usize,
    action: Action,
) {
    let Some(dev) = shared
        .lock()
        .ok()
        .and_then(|s| s.devices.get(selected).cloned())
    else {
        return;
    };
    let cmd = match action {
        Action::Connect => Command::Connect(dev.addr),
        Action::Disconnect => Command::Disconnect(dev.addr),
        Action::Remove => Command::Remove(dev.addr),
    };
    let _ = tx.send(cmd);
}

fn draw_text(
    canvas: &mut Canvas<Window>,
    tc: &TextureCreator<WindowContext>,
    // sdl2's Font carries two lifetimes (Font<'ttf, 'r>); elide both here.
    font: &Font<'_, '_>,
    text: &str,
    x: i32,
    y: i32,
    color: Color,
) {
    if text.is_empty() {
        return;
    }
    let Ok(surface) = font.render(text).blended(color) else { return };
    let Ok(texture) = tc.create_texture_from_surface(&surface) else { return };
    let q = texture.query();
    let _ = canvas.copy(&texture, None, Rect::new(x, y, q.width, q.height));
}

/// Find a usable TTF font. Override with BTPAIR_FONT=/path/to/font.ttf.
///
/// NOTE: the hardcoded candidate paths below are Debian/Ubuntu/Pop-flavoured.
/// For distro-agnostic behaviour, resolve a font via fontconfig at runtime
/// instead of guessing paths, e.g.:
///
///     let path = std::process::Command::new("fc-match")
///         .args(["-f", "%{file}", "sans"])
///         .output().ok()
///         .and_then(|o| String::from_utf8(o.stdout).ok())
///         .filter(|s| !s.is_empty());
///
/// fontconfig (`fc-match`) ships on effectively every desktop Linux, so this
/// removes the last distro-specific assumption. Alternatively embed a font
/// with include_bytes! and load it from memory for a zero-dependency build.
fn find_font() -> String {
    if let Ok(p) = std::env::var("BTPAIR_FONT") {
        return p;
    }
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];
    for c in CANDIDATES {
        if std::path::Path::new(c).exists() {
            return c.to_string();
        }
    }
    // Last resort: let SDL_ttf fail loudly with a clear path.
    CANDIDATES[0].to_string()
}
