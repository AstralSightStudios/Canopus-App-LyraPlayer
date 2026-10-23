//! Easing for the device's animations, in fixed point (`ONE` = 1.0; the watch has no FPU).
//! Every value is derived from the time since an animation began, so a slow frame skips ahead
//! instead of stretching the motion.

pub const ONE: i32 = 1024;

/// Linear progress through `duration` ms that started at `start`, after `delay` ms.
pub fn progress(start: u32, now: u32, delay: u32, duration: u32) -> i32 {
    let elapsed = now.wrapping_sub(start);
    if elapsed <= delay {
        return 0;
    }
    let elapsed = elapsed - delay;
    if elapsed >= duration || duration == 0 {
        ONE
    } else {
        (elapsed as i64 * ONE as i64 / duration as i64) as i32
    }
}

/// Fast start, gentle landing.
pub fn ease_out(t: i32) -> i32 {
    let u = (ONE - t) as i64;
    ONE - (u * u * u / (ONE as i64 * ONE as i64)) as i32
}

/// Gentle start, fast finish: for things leaving.
pub fn ease_in(t: i32) -> i32 {
    let t = t as i64;
    (t * t * t / (ONE as i64 * ONE as i64)) as i32
}

/// Slow, fast, slow.
pub fn ease_in_out(t: i32) -> i32 {
    if t < ONE / 2 {
        ease_in(t * 2) / 2
    } else {
        ONE / 2 + ease_out(t * 2 - ONE) / 2
    }
}

/// An ease-out that overshoots by a few percent and settles back, like a light spring.
pub fn ease_out_back(t: i32) -> i32 {
    // 1 + c3 (t - 1)^3 + c1 (t - 1)^2 with c1 = 1.4, c3 = c1 + 1 (in thousandths).
    let s = (t - ONE) as i64;
    let one = ONE as i64;
    let cubic = 2_400 * s * s * s / (one * one);
    let square = 1_400 * s * s / one;
    (one + (cubic + square) / 1_000) as i32
}

/// `from` → `to` at eased progress `e`.
pub fn mix(from: i32, to: i32, e: i32) -> i32 {
    from + ((to - from) as i64 * e as i64 / ONE as i64) as i32
}

pub fn mix_rgb(from: u32, to: u32, e: i32) -> u32 {
    let channel = |shift: u32| {
        let value = mix(
            ((from >> shift) & 0xFF) as i32,
            ((to >> shift) & 0xFF) as i32,
            e,
        );
        (value.clamp(0, 255) as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_at_zero_and_end_at_one() {
        for curve in [ease_out, ease_in, ease_in_out, ease_out_back] {
            assert_eq!(curve(0), 0);
            assert_eq!(curve(ONE), ONE);
        }
        assert!(ease_out(ONE / 4) > ONE / 4 && ease_in(ONE / 4) < ONE / 4);
        let peak = (0..=ONE).map(ease_out_back).max().unwrap();
        assert!(
            peak > ONE && peak < ONE * 108 / 100,
            "a light overshoot, got {peak}"
        );
    }

    #[test]
    fn progress_waits_for_the_delay_and_clamps() {
        assert_eq!(progress(1_000, 1_050, 100, 200), 0);
        assert_eq!(progress(1_000, 1_200, 100, 200), ONE / 2);
        assert_eq!(progress(1_000, 9_000, 100, 200), ONE);
        // The millisecond clock wraps.
        assert_eq!(progress(u32::MAX - 9, 90, 0, 200), ONE / 2);
    }
}
