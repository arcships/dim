//! Paces a streaming answer onto the screen at a steady rate.
//!
//! Models send text in bursts: a few hundred characters at once, then a
//! pause. Showing each burst as it lands reads as stop and go. Instead the
//! reveal follows the stream's *rate*: an average of what arrived over the
//! last ~0.6 s, plus a correction that keeps about 0.3 s of text in reserve.
//! The reserve bridges the pauses between bursts, so the text advances at an
//! even speed that tracks the model's real throughput (slow at 20 tokens a
//! second, fast at 600). When the answer has ended, what is left drains
//! within ~0.4 s.

use std::time::Instant;

/// How far back the arrival rate looks, in seconds.
const RATE_WINDOW: f64 = 0.6;
/// The text kept in reserve, in seconds of the arrival rate.
const LAG: f64 = 0.3;
/// How quickly the reserve is brought back to `LAG`, in seconds.
const CATCH_UP: f64 = 0.6;
/// What is left once the answer ended shows within this, in seconds.
const DRAIN: f64 = 0.4;
/// Slowest pace while text waits, in characters a second.
const MIN_RATE: f64 = 30.0;

/// The reveal of one streaming answer.
#[derive(Debug, Clone)]
pub struct Reveal {
    /// Characters shown.
    shown: usize,
    /// Fractions of a character owed to the next frame.
    carry: f64,
    /// Average arrival rate, characters a second.
    rate: f64,
    /// Characters the stream had at the last frame.
    seen: usize,
    last: Instant,
}

impl Reveal {
    pub fn new(now: Instant) -> Self {
        Self {
            shown: 0,
            carry: 0.0,
            rate: 0.0,
            seen: 0,
            last: now,
        }
    }

    pub fn shown(&self) -> usize {
        self.shown
    }

    /// Moves on to `now` for a stream of `total` characters; `ended` once no
    /// more will come. Returns the characters to show.
    pub fn advance(&mut self, now: Instant, total: usize, ended: bool) -> usize {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        if dt > 0.0 {
            let arrived = total.saturating_sub(self.seen) as f64;
            let alpha = 1.0 - (-dt / RATE_WINDOW).exp();
            self.rate += alpha * (arrived / dt - self.rate);
        }
        self.seen = total;
        let waiting = total.saturating_sub(self.shown) as f64;
        if waiting == 0.0 {
            self.carry = 0.0;
            return self.shown;
        }
        let mut speed = self.rate + (waiting - self.rate * LAG) / CATCH_UP;
        if ended {
            speed = speed.max(waiting / DRAIN);
        }
        self.carry += speed.max(MIN_RATE) * dt;
        let step = (self.carry.floor() as usize).min(total - self.shown);
        self.carry -= step as f64;
        self.shown += step;
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Frames of 16 ms over a stream that lands 120 characters every 400 ms
    /// (300 characters a second, in bursts).
    fn bursty() -> Vec<usize> {
        let start = Instant::now();
        let mut reveal = Reveal::new(start);
        let mut steps = Vec::new();
        let mut before = 0;
        for frame in 1..=250 {
            let ms = frame * 16;
            let total = (ms as usize / 400 + 1) * 120;
            let shown = reveal.advance(start + Duration::from_millis(ms), total, false);
            steps.push(shown - before);
            before = shown;
        }
        steps
    }

    #[test]
    fn bursts_reveal_at_an_even_pace_near_the_stream_rate() {
        let steps = bursty();
        // Once the rate is learnt (after ~1.5 s), every frame advances and
        // the pace stays near 300 characters a second (~4.8 a frame).
        let settled = &steps[100..];
        assert!(settled.iter().all(|step| *step > 0), "{settled:?}");
        let mean = settled.iter().sum::<usize>() as f64 / settled.len() as f64;
        assert!((3.5..6.5).contains(&mean), "{mean}");
        let spread = settled.iter().max().unwrap() - settled.iter().min().unwrap();
        assert!(spread <= 6, "{settled:?}");
    }

    #[test]
    fn an_ended_stream_drains_quickly() {
        let start = Instant::now();
        let mut reveal = Reveal::new(start);
        reveal.advance(start + Duration::from_millis(16), 1_000, true);
        let shown = reveal.advance(start + Duration::from_millis(450), 1_000, true);
        assert_eq!(shown, 1_000);
    }
}
