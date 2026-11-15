//! Gamepad input read straight from Linux evdev (`/dev/input/event*`).
//!
//! No libudev or SDL, so the musl build stays static. On a Steam Deck in Game Mode, Steam
//! exposes the controller as a virtual Xbox 360 pad that the `deck` user can read. On other
//! systems (or without a pad) this finds nothing and stays idle.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pad {
    A,
    B,
    X,
    Y,
    LB,
    RB,
    LT,
    RT,
    View,
    Menu,
    L3,
    R3,
    Up,
    Down,
    Left,
    Right,
}

impl Pad {
    fn repeats(self) -> bool {
        matches!(self, Pad::Up | Pad::Down | Pad::Left | Pad::Right | Pad::LT | Pad::RT)
    }
}

const REPEAT_DELAY: Duration = Duration::from_millis(350);
const REPEAT_EVERY: Duration = Duration::from_millis(70);

pub struct Gamepad {
    rx: Receiver<(Pad, bool)>,
    open: Arc<Mutex<HashSet<PathBuf>>>,
    held: Option<(Pad, Instant)>,
}

impl Gamepad {
    /// Starts watching for pads in the background. Cheap no-op where `/dev/input` is absent.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let open = Arc::new(Mutex::new(HashSet::new()));
        if Path::new("/dev/input").is_dir() {
            let open = open.clone();
            thread::spawn(move || {
                loop {
                    scan(&tx, &open);
                    thread::sleep(Duration::from_secs(2));
                }
            });
        }
        Gamepad { rx, open, held: None }
    }

    pub fn connected(&self) -> bool {
        !self.open.lock().unwrap().is_empty()
    }

    /// Next button press, including auto-repeat for held directions and triggers.
    pub fn poll(&mut self) -> Option<Pad> {
        while let Ok((p, down)) = self.rx.try_recv() {
            if down {
                self.held = p.repeats().then(|| (p, Instant::now() + REPEAT_DELAY));
                return Some(p);
            }
            if self.held.is_some_and(|(h, _)| h == p) {
                self.held = None;
            }
        }
        if let Some((p, at)) = self.held
            && Instant::now() >= at
        {
            self.held = Some((p, at + REPEAT_EVERY));
            return Some(p);
        }
        None
    }
}

fn scan(tx: &Sender<(Pad, bool)>, open: &Arc<Mutex<HashSet<PathBuf>>>) {
    let Ok(dir) = fs::read_dir("/dev/input") else { return };
    for e in dir.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") || open.lock().unwrap().contains(&path) || !is_gamepad(&name) {
            continue;
        }
        let Ok(f) = File::open(&path) else { continue };
        open.lock().unwrap().insert(path.clone());
        let (tx, open) = (tx.clone(), open.clone());
        thread::spawn(move || {
            read_events(f, &tx);
            open.lock().unwrap().remove(&path); // unplugged (or the virtual pad went away)
        });
    }
}

const BTN_SOUTH: usize = 0x130;

/// A gamepad advertises BTN_SOUTH (A). Steam's trackpad keyboard/mouse devices don't.
fn is_gamepad(event: &str) -> bool {
    let Ok(caps) = fs::read_to_string(format!("/sys/class/input/{event}/device/capabilities/key")) else {
        return false;
    };
    // Space-separated hex words, most significant first, each the kernel's `long` (64-bit here).
    let words: Vec<&str> = caps.split_whitespace().collect();
    let idx = BTN_SOUTH / 64;
    idx < words.len() && u64::from_str_radix(words[words.len() - 1 - idx], 16).is_ok_and(|w| w >> (BTN_SOUTH % 64) & 1 == 1)
}

const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;

#[derive(Default)]
struct State {
    hat: (i32, i32),
    stick: (i32, i32),
    lt: bool,
    rt: bool,
}

fn read_events(mut f: File, tx: &Sender<(Pad, bool)>) {
    // struct input_event on 64-bit Linux: timeval (16 bytes), type u16, code u16, value i32.
    let mut buf = [0u8; 24];
    let mut st = State::default();
    while f.read_exact(&mut buf).is_ok() {
        let ty = u16::from_ne_bytes([buf[16], buf[17]]);
        let code = u16::from_ne_bytes([buf[18], buf[19]]);
        let value = i32::from_ne_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let mut send = |p, down| tx.send((p, down)).is_ok();
        let ok = match ty {
            EV_KEY if value != 2 => match button(code) {
                Some(p) => send(p, value == 1),
                None => true,
            },
            EV_ABS => abs(&mut st, code, value, &mut send),
            _ => true,
        };
        if !ok {
            return;
        }
    }
}

fn button(code: u16) -> Option<Pad> {
    // Codes as the Linux xpad driver and Steam's virtual pad report them.
    Some(match code {
        0x130 => Pad::A,
        0x131 => Pad::B,
        0x133 => Pad::X,
        0x134 => Pad::Y,
        0x136 => Pad::LB,
        0x137 => Pad::RB,
        0x13a => Pad::View,
        0x13b => Pad::Menu,
        0x13d => Pad::L3,
        0x13e => Pad::R3,
        0x220 => Pad::Up, // BTN_DPAD_*, for pads that report the d-pad as buttons
        0x221 => Pad::Down,
        0x222 => Pad::Left,
        0x223 => Pad::Right,
        _ => return None,
    })
}

/// Turns an axis into a -1/0/1 direction, with hysteresis so a stick near the threshold doesn't chatter.
fn stick_dir(prev: i32, v: i32) -> i32 {
    const ON: i32 = 20_000;
    const OFF: i32 = 10_000;
    match prev {
        0 if v >= ON => 1,
        0 if v <= -ON => -1,
        1 if v < OFF => {
            if v <= -ON {
                -1
            } else {
                0
            }
        }
        -1 if v > -OFF => i32::from(v >= ON),
        p => p,
    }
}

fn axis_change(prev: i32, now: i32, neg: Pad, pos: Pad, send: &mut impl FnMut(Pad, bool) -> bool) -> bool {
    if prev == now {
        return true;
    }
    let ok = match prev {
        -1 => send(neg, false),
        1 => send(pos, false),
        _ => true,
    };
    ok && match now {
        -1 => send(neg, true),
        1 => send(pos, true),
        _ => true,
    }
}

fn abs(st: &mut State, code: u16, v: i32, send: &mut impl FnMut(Pad, bool) -> bool) -> bool {
    match code {
        0x10 => {
            let now = v.signum();
            let ok = axis_change(st.hat.0, now, Pad::Left, Pad::Right, send);
            st.hat.0 = now;
            ok
        }
        0x11 => {
            let now = v.signum();
            let ok = axis_change(st.hat.1, now, Pad::Up, Pad::Down, send);
            st.hat.1 = now;
            ok
        }
        0x00 => {
            let now = stick_dir(st.stick.0, v);
            let ok = axis_change(st.stick.0, now, Pad::Left, Pad::Right, send);
            st.stick.0 = now;
            ok
        }
        0x01 => {
            let now = stick_dir(st.stick.1, v);
            let ok = axis_change(st.stick.1, now, Pad::Up, Pad::Down, send);
            st.stick.1 = now;
            ok
        }
        // Triggers: 0..255 on xpad; the thresholds also work for wider ranges.
        0x02 | 0x05 => {
            let (held, pad) = if code == 0x02 { (&mut st.lt, Pad::LT) } else { (&mut st.rt, Pad::RT) };
            let now = if *held { v > 40 } else { v > 100 };
            if now == *held {
                return true;
            }
            *held = now;
            send(pad, now)
        }
        _ => true,
    }
}

/// Opens Steam's on-screen keyboard when running under Steam's Game Mode UI.
pub fn open_steam_keyboard() {
    if std::env::var_os("SteamGamepadUI").is_none() && std::env::var_os("SteamDeck").is_none() {
        return;
    }
    if let Ok(mut child) = std::process::Command::new("steam")
        .arg("steam://open/keyboard")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        thread::spawn(move || child.wait());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_hysteresis() {
        assert_eq!(stick_dir(0, 15_000), 0);
        assert_eq!(stick_dir(0, 25_000), 1);
        assert_eq!(stick_dir(1, 15_000), 1);
        assert_eq!(stick_dir(1, 5_000), 0);
        assert_eq!(stick_dir(1, -30_000), -1);
        assert_eq!(stick_dir(-1, -15_000), -1);
    }

    #[test]
    fn hat_press_release() {
        let mut st = State::default();
        let mut got = vec![];
        let mut send = |p, d| {
            got.push((p, d));
            true
        };
        abs(&mut st, 0x11, 1, &mut send);
        abs(&mut st, 0x11, 0, &mut send);
        abs(&mut st, 0x10, -1, &mut send);
        abs(&mut st, 0x10, 1, &mut send);
        assert_eq!(got, vec![(Pad::Down, true), (Pad::Down, false), (Pad::Left, true), (Pad::Left, false), (Pad::Right, true)]);
    }
}
