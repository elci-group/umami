//! The Umami control policy: a pure state machine over pressure samples.
//!
//! States: Calm -> Pressured -> Critical, plus ThrashGuard, a protective
//! backoff entered immediately when swap-in saturation and full memory
//! stalls coincide (flash tier is the bottleneck, stop feeding it).
//! Escalation requires `enter_samples` consecutive demanding samples;
//! relaxation requires `exit_samples` quiet ones (hysteresis).

use crate::config::PolicyCfg;
use crate::procfs::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Calm,
    Pressured,
    Critical,
    ThrashGuard,
}

impl State {
    /// Order used for escalation decisions. ThrashGuard sits outside the
    /// ladder; it is entered and exited through its own rules.
    fn severity(self) -> u8 {
        match self {
            State::Calm => 0,
            State::Pressured => 1,
            State::Critical => 2,
            State::ThrashGuard => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub state: State,
    pub swappiness: u32,
    pub cache_pressure: u32,
    /// True when this evaluation changed the state.
    pub changed: bool,
    pub reason: String,
}

pub struct Policy {
    cfg: PolicyCfg,
    state: State,
    streak: u32,
    /// Direction the streak is building toward: +1 escalating, -1 relaxing.
    streak_dir: i8,
}

impl Policy {
    pub fn new(cfg: PolicyCfg) -> Policy {
        Policy {
            cfg,
            state: State::Calm,
            streak: 0,
            streak_dir: 0,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> State {
        self.state
    }

    pub fn evaluate(&mut self, s: &Sample) -> Decision {
        let prev = self.state;

        let thrashing = s.swapin_kbs >= self.cfg.thrash_swapin_kbs
            && s.psi_full_avg10 >= self.cfg.thrash_psi_full_avg10;

        // Severity demanded by this sample alone.
        let (mut target, mut why) = (State::Calm, String::new());
        if s.psi_some_avg10 >= self.cfg.psi_pressure_threshold {
            target = State::Pressured;
            why = format!(
                "psi_some_avg10 {:.1} >= {:.1}",
                s.psi_some_avg10, self.cfg.psi_pressure_threshold
            );
        }
        if s.mem_available_pct <= self.cfg.mem_pressure_available_pct {
            target = State::Pressured;
            why = format!(
                "mem_available {:.1}% <= {:.1}%",
                s.mem_available_pct, self.cfg.mem_pressure_available_pct
            );
        }
        if s.psi_some_avg10 >= self.cfg.psi_critical_threshold {
            target = State::Critical;
            why = format!(
                "psi_some_avg10 {:.1} >= {:.1}",
                s.psi_some_avg10, self.cfg.psi_critical_threshold
            );
        }
        if s.mem_available_pct <= self.cfg.mem_critical_available_pct {
            target = State::Critical;
            why = format!(
                "mem_available {:.1}% <= {:.1}%",
                s.mem_available_pct, self.cfg.mem_critical_available_pct
            );
        }

        let reason;
        if thrashing {
            self.state = State::ThrashGuard;
            self.streak = 0;
            self.streak_dir = 0;
            reason = format!(
                "thrash guard: swap-in {:.0} KiB/s with psi_full_avg10 {:.1}",
                s.swapin_kbs, s.psi_full_avg10
            );
        } else if self.state == State::ThrashGuard {
            // Leave the guard only after the system has been calm for a while.
            if target == State::Calm {
                self.streak += 1;
                if self.streak >= self.cfg.exit_samples {
                    self.state = State::Calm;
                    self.streak = 0;
                    reason = "guard released: system calm".to_string();
                } else {
                    reason = format!("guard cooling down ({}/{})", self.streak, self.cfg.exit_samples);
                }
            } else {
                self.streak = 0;
                reason = "guard held: pressure persists".to_string();
            }
        } else if target.severity() > self.state.severity() {
            self.count(1);
            if self.streak >= self.cfg.enter_samples {
                self.state = target;
                self.streak = 0;
                self.streak_dir = 0;
                reason = format!("escalating: {}", why);
            } else {
                reason = format!(
                    "pressure rising ({}/{}): {}",
                    self.streak, self.cfg.enter_samples, why
                );
            }
        } else if target.severity() < self.state.severity() {
            self.count(-1);
            if self.streak >= self.cfg.exit_samples {
                self.state = target;
                self.streak = 0;
                self.streak_dir = 0;
                reason = "relaxing".to_string();
            } else {
                reason = format!("cooling down ({}/{})", self.streak, self.cfg.exit_samples);
            }
        } else {
            self.streak = 0;
            self.streak_dir = 0;
            reason = "steady state".to_string();
        }

        let (swappiness, cache_pressure) = match self.state {
            State::Calm => (self.cfg.swappiness_calm, self.cfg.cache_pressure_calm),
            State::Pressured => (self.cfg.swappiness_pressured, self.cfg.cache_pressure_pressured),
            State::Critical => (self.cfg.swappiness_critical, self.cfg.cache_pressure_pressured),
            State::ThrashGuard => (
                self.cfg.swappiness_thrash_guard,
                self.cfg.cache_pressure_pressured,
            ),
        };

        Decision {
            state: self.state,
            swappiness,
            cache_pressure,
            changed: self.state != prev,
            reason,
        }
    }

    fn count(&mut self, dir: i8) {
        if self.streak_dir == dir {
            self.streak += 1;
        } else {
            self.streak_dir = dir;
            self.streak = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> PolicyCfg {
        Config::from_str("")
            .unwrap()
            .policy()
    }

    use crate::config::Config;

    fn sample(avail_pct: f64, psi_some: f64, psi_full: f64, swapin: f64) -> Sample {
        Sample {
            mem_available_pct: avail_pct,
            swap_used_pct: 0.0,
            psi_some_avg10: psi_some,
            psi_full_avg10: psi_full,
            swapin_kbs: swapin,
            swapout_kbs: 0.0,
        }
    }

    #[test]
    fn stays_calm_on_quiet_system() {
        let mut p = Policy::new(test_cfg());
        for _ in 0..20 {
            let d = p.evaluate(&sample(50.0, 0.5, 0.0, 0.0));
            assert_eq!(d.state, State::Calm);
            assert_eq!(d.swappiness, 60);
            assert!(!d.changed);
        }
    }

    #[test]
    fn escalates_only_after_enter_samples() {
        let mut p = Policy::new(test_cfg());
        let d1 = p.evaluate(&sample(50.0, 8.0, 1.0, 0.0)); // pressure, 1/2
        assert_eq!(d1.state, State::Calm);
        assert!(!d1.changed);
        let d2 = p.evaluate(&sample(50.0, 8.0, 1.0, 0.0)); // pressure, 2/2
        assert_eq!(d2.state, State::Pressured);
        assert!(d2.changed);
        assert_eq!(d2.swappiness, 120);
        assert_eq!(d2.cache_pressure, 50);
    }

    #[test]
    fn spike_does_not_escalate() {
        let mut p = Policy::new(test_cfg());
        p.evaluate(&sample(50.0, 8.0, 1.0, 0.0)); // 1/2
        let d = p.evaluate(&sample(50.0, 0.0, 0.0, 0.0)); // quiet again
        assert_eq!(d.state, State::Calm);
        let d = p.evaluate(&sample(50.0, 8.0, 1.0, 0.0)); // fresh streak 1/2
        assert_eq!(d.state, State::Calm);
    }

    #[test]
    fn critical_maps_to_max_swappiness() {
        let mut p = Policy::new(test_cfg());
        p.evaluate(&sample(3.0, 25.0, 10.0, 0.0));
        let d = p.evaluate(&sample(3.0, 25.0, 10.0, 0.0));
        assert_eq!(d.state, State::Critical);
        assert_eq!(d.swappiness, 180);
    }

    #[test]
    fn relaxes_only_after_exit_samples() {
        let mut p = Policy::new(test_cfg());
        p.evaluate(&sample(50.0, 8.0, 1.0, 0.0));
        p.evaluate(&sample(50.0, 8.0, 1.0, 0.0));
        assert_eq!(p.state(), State::Pressured);
        for i in 1..10 {
            let d = p.evaluate(&sample(50.0, 0.0, 0.0, 0.0));
            assert_eq!(d.state, State::Pressured, "sample {} should still be pressured", i);
        }
        let d = p.evaluate(&sample(50.0, 0.0, 0.0, 0.0));
        assert_eq!(d.state, State::Calm);
        assert_eq!(d.swappiness, 60);
    }

    #[test]
    fn thrash_guard_enters_immediately_and_backs_off() {
        let mut p = Policy::new(test_cfg());
        // Sustained pressure first, then the flash tier saturates.
        p.evaluate(&sample(10.0, 8.0, 6.0, 100.0));
        let d = p.evaluate(&sample(10.0, 8.0, 6.0, 60000.0));
        assert_eq!(d.state, State::ThrashGuard);
        assert!(d.changed);
        assert_eq!(d.swappiness, 30); // backoff: stop feeding the flash tier
    }

    #[test]
    fn thrash_guard_needs_calm_streak_to_release() {
        let mut p = Policy::new(test_cfg());
        p.evaluate(&sample(10.0, 8.0, 6.0, 60000.0));
        assert_eq!(p.state(), State::ThrashGuard);
        // Pressure persists: guard must not release.
        let d = p.evaluate(&sample(10.0, 8.0, 1.0, 0.0));
        assert_eq!(d.state, State::ThrashGuard);
        // Now calm: requires exit_samples before release.
        for _ in 0..9 {
            assert_eq!(p.evaluate(&sample(50.0, 0.0, 0.0, 0.0)).state, State::ThrashGuard);
        }
        assert_eq!(p.evaluate(&sample(50.0, 0.0, 0.0, 0.0)).state, State::Calm);
    }
}
