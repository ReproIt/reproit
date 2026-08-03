//! A fixed timestep loop on a real third-party engine runner: bevy_app's
//! ScheduleRunnerPlugin drives the frame schedule, bevy_ecs runs the game
//! system, and input arrives on stdin because a container has no evdev or
//! window server and an engine that cannot run headless cannot be tested.
//!
//! The planted defect is the STALE COMBO from validation/process/engine.c:
//! the code assumes a combo's presses arrive close together, so a press
//! arriving more than STALE_AFTER frames after the previous one reuses a
//! stale slot and panics. The same bytes BACK TO BACK are safe, so a replay
//! that ignored the recorded tick schedule would not reproduce the crash;
//! the bug is in the schedule, not the bytes.

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Update};
use bevy_ecs::event::EventWriter;
use bevy_ecs::system::Local;
use std::time::Duration;

const STALE_AFTER: u32 = 6;

#[derive(Default)]
struct EngineState {
    frame: u32,
    budget: u32,
    fixed: bool,
    last_press: u32,
    have_press: bool,
    combo: u32,
}

/// One nonblocking byte off fd 0. Read through libc so the boundary sees the
/// same call an SDL or terminal front end would make; std's stdin buffers
/// 8 KiB at a time, which would swallow the whole session in one frame.
fn poll_input() -> Option<u8> {
    let mut byte = 0u8;
    let got = unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) };
    (got == 1).then_some(byte)
}

fn frame(mut state: Local<EngineState>, mut exit: EventWriter<AppExit>) {
    if state.budget == 0 {
        state.budget = std::env::var("ENGINE_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);
        state.fixed = std::env::var("REPROIT_FIXED").is_ok();
    }
    if let Some(b'u') = poll_input() {
        let gap = if state.have_press {
            state.frame - state.last_press
        } else {
            0
        };
        if state.have_press && gap > STALE_AFTER {
            if state.fixed {
                state.combo = 0; // the fix: a stale combo is discarded
            } else {
                // the defect: the slot is reused while stale
                state.combo += 1;
                println!("frame {} stale gap {} combo {}", state.frame, gap, state.combo);
                assert!(gap <= STALE_AFTER, "stale combo slot reused");
            }
        }
        state.combo += 1;
        state.last_press = state.frame;
        state.have_press = true;
        println!("frame {} press combo {}", state.frame, state.combo);
    }
    state.frame += 1;
    if state.frame >= state.budget {
        println!("survived");
        exit.write(AppExit::Success);
    }
}

fn main() {
    unsafe {
        let flags = libc::fcntl(0, libc::F_GETFL);
        libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    App::new()
        .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(5)))
        .add_systems(Update, frame)
        .run();
}
