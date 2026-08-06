//! The determinism seam application code routes time and randomness through
//! (feature `instrument`).
//!
//! Route a clock read through [`now_millis`] and a random draw through the
//! [`SeededRng`] this module hands out, and both become deterministic under
//! hermetic replay: the clock returns the capture's `observedAtMs` instant and
//! the RNG replays a fixed xorshift64* stream from the capture's `replaySeed`.
//! Outside replay the clock reads the real wall clock and no seeded RNG is
//! offered, so capture-time behavior is unchanged.
//!
//! This is the honest boundary, stated plainly. Rust has no monkeypatching, so
//! a direct `std::time::SystemTime::now()` or `rand::random()` /  `getrandom`
//! call in application or library code CANNOT be intercepted and stays
//! unpinned. Only reads routed through this seam are deterministic. The seam
//! reuses the replay session (it reads the loaded capture's envelope), so it is
//! part of the `instrument` feature, not a standalone one.
//!
//! Reproducibility scope, without overclaim: the seed makes REPLAY runs
//! deterministic, run to run. It reproduces the EXACT production draw only when
//! the capture recorded the seed the app actually used (the app drew through
//! this same seam at capture time and the seed rode the envelope). A capture
//! whose `replaySeed` was synthesized by the SDK gives a deterministic replay
//! stream, not the original production numbers.
//!
//! The stream is the ONE shape the whole SDK uses: the Node reference's
//! `Math.random` replacement (`replay.js` `pinEnvelope`) draws the same
//! xorshift64*, and [`crate::instrument::ReplayRng`] is this exact type.

use serde_json::Value;

/// A deterministic xorshift64* stream. Built from the capture's `replaySeed`
/// under replay (see [`replay_rng`]), or from an explicit seed the application
/// supplies. The state is forced odd, so the stream never degenerates to zero.
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// A stream from a raw 64-bit seed.
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    /// A stream from the envelope's 16-hex-digit `replaySeed`. `None` when the
    /// text is not hex. Mirrors the Node reference's
    /// `BigInt('0x' + replaySeed.slice(0, 16))`.
    pub fn from_seed_hex(seed: &str) -> Option<Self> {
        let hex: String = seed.chars().take(16).collect();
        let state = u64::from_str_radix(&hex, 16).ok()?;
        Some(Self::from_seed(state))
    }

    /// The next raw 64-bit draw of the xorshift64* stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// The next draw in [0, 1): the top 53 bits of [`next_u64`], byte-identical
    /// to the Node SDK's `Math.random` replacement.
    ///
    /// [`next_u64`]: Self::next_u64
    pub fn next_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// The process clock through the determinism seam: real wall-clock time in
/// capture mode, pinned to the capture's `observedAtMs` instant in replay mode.
/// Named limitation: a direct `SystemTime::now()` call reads the real clock;
/// only reads routed here are pinned.
pub fn now_millis() -> u64 {
    crate::replay::now_millis()
}

/// True when this process serves a recorded capture (replay mode).
pub fn replaying() -> bool {
    crate::instrument::replaying()
}

/// The seeded stream for this replay, from the capture envelope's `replaySeed`.
/// `None` outside replay mode or when the capture carries no seed. Two calls in
/// one replay yield the SAME starting stream, so independent code paths that
/// each take a fresh RNG stay reproducible.
pub fn replay_rng() -> Option<SeededRng> {
    let envelope = crate::instrument::replay_envelope()?;
    let seed = envelope.get("replaySeed").and_then(Value::as_str)?;
    SeededRng::from_seed_hex(seed)
}
