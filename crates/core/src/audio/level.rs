pub fn dbfs(level: f32) -> f32 {
    if level <= 1e-6 {
        -120.0
    } else {
        20.0 * level.log10()
    }
}

/// Warns once when `secs` consecutive one-second levels stay below the threshold: the sign
/// that Zoom's Speaker is no longer the loopback device (spec §4.3).
pub struct SilenceWatch {
    threshold_dbfs: f32,
    needed: u32,
    run: u32,
}

impl SilenceWatch {
    pub fn new(threshold_dbfs: f32, secs: u32) -> Self {
        Self { threshold_dbfs, needed: secs, run: 0 }
    }

    pub fn observe(&mut self, level: f32) -> bool {
        if dbfs(level) >= self.threshold_dbfs {
            self.run = 0;
            return false;
        }
        self.run += 1;
        self.run == self.needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    #[test]
    fn warns_once_after_the_silent_run_and_again_after_signal_returns() {
        let mut w = SilenceWatch::new(-60.0, 3);
        assert!(!w.observe(lin(-80.0)));
        assert!(!w.observe(lin(-80.0)));
        assert!(w.observe(lin(-80.0)));
        assert!(!w.observe(0.0), "one warning per silent stretch");
        assert!(!w.observe(lin(-30.0)));
        assert!(!w.observe(lin(-90.0)));
        assert!(!w.observe(lin(-90.0)));
        assert!(w.observe(lin(-90.0)));
    }

    #[test]
    fn dbfs_of_silence_is_floored() {
        assert_eq!(dbfs(0.0), -120.0);
        assert!((dbfs(1.0)).abs() < 1e-6);
    }
}
