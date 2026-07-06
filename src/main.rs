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

use sdl2::controller::{Axis, Button};
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
const LEGEND_BG: Color = Color::RGB(24, 24, 30);
const EXIT_BG: Color = Color::RGB(150, 54, 54);
const EXIT_TEXT: Color = Color::RGB(255, 235, 235);

// Left-stick navigation tuning (axis range is -32768..32767; ~60fps loop).
const STICK_DEADZONE: i16 = 16000;
const STICK_REPEAT_DELAY: i32 = 18;
const STICK_REPEAT_RATE: i32 = 6;

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

    // Scale the whole UI to the actual screen so it's legible on a TV from the
    // couch. Baseline design is 720p; everything grows proportionally on 1080p/4K.
    let (_screen_w, screen_h) = canvas.output_size().unwrap_or((1280, 720));
    let scale = (screen_h as f32 / 720.0).max(1.0);
    let fsize = |base: f32| ((base * scale) as u16).max(1);

    let font_path = find_font();
    let font_title = ttf.load_font(&font_path, fsize(52.0)).expect("load title font");
    let font = ttf.load_font(&font_path, fsize(30.0)).expect("load font");
    let font_small = ttf.load_font(&font_path, fsize(22.0)).expect("load small font");
    let font_legend = ttf.load_font(&font_path, fsize(32.0)).expect("load legend font");

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
    let mut stick_neutral = true;
    let mut stick_cooldown: i32 = 0;
    // On-screen Exit button, recomputed each frame; mouse clicks are tested
    // against last frame's rect (fine at 60fps since it never moves).
    let mut exit_btn = Rect::new(0, 0, 0, 0);

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

                Event::MouseButtonDown { x, y, .. } => {
                    if exit_btn.contains_point((x, y)) {
                        break 'running;
                    }
                }

                _ => {}
            }
        }

        // Left-stick vertical navigation, mirroring the D-pad with auto-repeat.
        if stick_cooldown > 0 {
            stick_cooldown -= 1;
        }
        let ly = controllers
            .iter()
            .map(|c| c.axis(Axis::LeftY))
            .max_by_key(|v| v.unsigned_abs())
            .unwrap_or(0);
        if ly.saturating_abs() > STICK_DEADZONE {
            if stick_cooldown == 0 {
                if ly > 0 {
                    selected += 1;
                } else {
                    selected = selected.saturating_sub(1);
                }
                stick_cooldown = if stick_neutral {
                    STICK_REPEAT_DELAY
                } else {
                    STICK_REPEAT_RATE
                };
                stick_neutral = false;
            }
        } else {
            stick_neutral = true;
            stick_cooldown = 0;
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

        let (w, h) = canvas.output_size().unwrap_or((1280, 720));
        let (w, h) = (w as i32, h as i32);
        // Scale layout metrics to the screen, mirroring the font scaling above.
        let px = |v: f32| (v * scale) as i32;
        let margin = px(48.0);

        // --- bottom legend bar (drawn first so we know how much vertical space
        // the device list may occupy) ---
        let legend_h = px(72.0);
        let legend_top = h - legend_h;
        canvas.set_draw_color(LEGEND_BG);
        let _ = canvas.fill_rect(Rect::new(0, legend_top, w as u32, legend_h as u32));
        let legend = "[A] Connect / Pair     [Y] Disconnect     [Back] Remove     [B] Exit";
        let (lw, lh) = font_legend.size_of(legend).unwrap_or((0, 0));
        draw_text(
            &mut canvas,
            &texture_creator,
            &font_legend,
            legend,
            (w - lw as i32) / 2,
            legend_top + (legend_h - lh as i32) / 2,
            TEXT,
        );

        // --- header: title + adapter/scan state ---
        draw_text(&mut canvas, &texture_creator, &font_title, "Bluetooth", margin, px(28.0), TEXT);
        let header = if snap.powered {
            format!("scanning{}", if snap.scanning { "…" } else { "" })
        } else {
            "adapter OFF".to_string()
        };
        draw_text(&mut canvas, &texture_creator, &font_small, &header, margin, px(92.0), DIM);

        // --- Exit button, top-right (mouse-clickable; B/Esc also exit) ---
        let (ew, eh) = font.size_of("Exit").unwrap_or((0, 0));
        let btn_w = ew as i32 + px(48.0);
        let btn_h = eh as i32 + px(20.0);
        exit_btn = Rect::new(w - margin - btn_w, px(28.0), btn_w as u32, btn_h as u32);
        canvas.set_draw_color(EXIT_BG);
        let _ = canvas.fill_rect(exit_btn);
        draw_text(
            &mut canvas,
            &texture_creator,
            &font,
            "Exit",
            exit_btn.x() + px(24.0),
            exit_btn.y() + px(10.0),
            EXIT_TEXT,
        );

        // --- Device rows ---
        let row_h = px(76.0);
        let list_top = px(128.0);
        for (i, d) in snap.devices.iter().enumerate() {
            let y = list_top + i as i32 * row_h;
            // Don't draw rows that would collide with the legend bar.
            if y + row_h > legend_top {
                break;
            }

            let selected_row = i == selected;
            if selected_row {
                canvas.set_draw_color(SELECT);
            } else {
                canvas.set_draw_color(PANEL);
            }
            let _ = canvas.fill_rect(Rect::new(margin, y, (w - margin * 2) as u32, (row_h - px(8.0)) as u32));

            let name_color = if d.connected { GREEN } else { TEXT };
            draw_text(&mut canvas, &texture_creator, &font, &d.name, margin + px(20.0), y + px(8.0), name_color);

            let mut tags = Vec::new();
            if d.connected {
                tags.push("connected".to_string());
            } else if d.paired {
                tags.push("paired".to_string());
            } else {
                tags.push("not paired".to_string());
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
                margin + px(20.0),
                y + px(42.0),
                DIM,
            );

            // Live action status shown inline on the selected row (right-aligned),
            // instead of at the bottom of the screen.
            if selected_row && !snap.status.is_empty() {
                let (sw, sh) = font_small.size_of(&snap.status).unwrap_or((0, 0));
                draw_text(
                    &mut canvas,
                    &texture_creator,
                    &font_small,
                    &snap.status,
                    w - margin - px(20.0) - sw as i32,
                    y + (row_h - px(8.0) - sh as i32) / 2,
                    TEXT,
                );
            }
        }

        if snap.devices.is_empty() {
            draw_text(
                &mut canvas,
                &texture_creator,
                &font,
                "No devices yet — put your device in pairing mode…",
                margin + px(20.0),
                list_top + px(8.0),
                DIM,
            );
        }

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
