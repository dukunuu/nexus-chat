//! What a tool call touches, declared before it runs, and the wave planner
//! that lets non-conflicting calls run together.

use std::path::{Path, PathBuf};

/// The paths a call reads and writes, and whether it does something no path
/// list can describe (running a program, mutating other state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effect {
    /// Canonical paths from `Workspace::resolve`.
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
    /// Runs a program, which can touch anything: a barrier.
    pub exec: bool,
    /// Mutates state we can't describe by path.
    pub barrier: bool,
    /// Reaches the network; informational for now.
    pub network: bool,
}

impl Effect {
    /// Touches nothing that could race.
    #[must_use]
    pub fn read_only() -> Self {
        Self::default()
    }

    /// Conflicts with every other call.
    #[must_use]
    pub fn barrier() -> Self {
        Self {
            barrier: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.writes.is_empty() && !self.exec && !self.barrier
    }

    /// Whether the two calls must keep their relative order: either is a
    /// barrier, or one writes a path the other reads or writes (the same
    /// path, or one inside the other, so a directory listing conflicts with
    /// a write to a file under it). Reads never conflict with reads.
    #[must_use]
    pub fn conflicts(&self, other: &Effect) -> bool {
        fn overlaps(a: &Path, b: &Path) -> bool {
            a.starts_with(b) || b.starts_with(a)
        }
        fn writes_into(writer: &Effect, other: &Effect) -> bool {
            writer.writes.iter().any(|write| {
                other
                    .reads
                    .iter()
                    .chain(&other.writes)
                    .any(|path| overlaps(write, path))
            })
        }
        self.exec
            || self.barrier
            || other.exec
            || other.barrier
            || writes_into(self, other)
            || writes_into(other, self)
    }
}

/// Group calls into waves that can each run concurrently. A call's wave is
/// one past the latest wave of any earlier call it conflicts with (0 if
/// none), so conflicting calls keep their order while an independent later
/// call may run before an earlier one. Waves are ascending; each lists its
/// calls in original order.
#[must_use]
pub fn plan_waves(effects: &[Effect]) -> Vec<Vec<usize>> {
    let mut wave_of: Vec<usize> = Vec::with_capacity(effects.len());
    let mut waves: Vec<Vec<usize>> = Vec::new();
    for (index, effect) in effects.iter().enumerate() {
        let wave = effects
            .iter()
            .zip(&wave_of)
            .filter(|(earlier, _)| effect.conflicts(earlier))
            .map(|(_, wave)| wave + 1)
            .max()
            .unwrap_or(0);
        wave_of.push(wave);
        if wave == waves.len() {
            waves.push(Vec::new());
        }
        waves[wave].push(index);
    }
    waves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str) -> Effect {
        Effect {
            reads: vec![PathBuf::from(path)],
            ..Effect::read_only()
        }
    }

    fn write(path: &str) -> Effect {
        Effect {
            writes: vec![PathBuf::from(path)],
            ..Effect::read_only()
        }
    }

    fn exec() -> Effect {
        Effect {
            exec: true,
            ..Effect::read_only()
        }
    }

    #[test]
    fn conflicts_only_where_a_write_meets_the_same_path_or_a_barrier() {
        let cases = [
            (read("/w/a"), read("/w/a"), false),
            (read("/w/src"), read("/w/src/a.rs"), false),
            (write("/w/a"), read("/w/a"), true),
            (write("/w/a"), write("/w/a"), true),
            (read("/w/src"), write("/w/src/a.rs"), true),
            (write("/w/src"), read("/w/src/a.rs"), true),
            (write("/w/a"), write("/w/b"), false),
            (write("/w/a"), read("/w/b"), false),
            (write("/w/src/a.rs"), read("/w/src/ab.rs"), false),
            (exec(), read("/w/a"), true),
            (exec(), Effect::read_only(), true),
            (Effect::barrier(), read("/w/a"), true),
            (Effect::barrier(), Effect::read_only(), true),
            (Effect::read_only(), Effect::read_only(), false),
        ];
        for (a, b, expected) in cases {
            assert_eq!(a.conflicts(&b), expected, "{a:?} vs {b:?}");
            assert_eq!(b.conflicts(&a), expected, "{b:?} vs {a:?}");
        }
    }

    #[test]
    fn read_only_means_no_writes_exec_or_barrier() {
        assert!(Effect::read_only().is_read_only());
        assert!(read("/w/a").is_read_only());
        assert!(
            Effect {
                network: true,
                ..Effect::read_only()
            }
            .is_read_only()
        );
        assert!(!write("/w/a").is_read_only());
        assert!(!exec().is_read_only());
        assert!(!Effect::barrier().is_read_only());
    }

    #[test]
    fn waves_keep_conflicting_calls_ordered_and_run_the_rest_together() {
        let reads: Vec<_> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|name| read(&format!("/w/{name}")))
            .collect();
        assert_eq!(plan_waves(&reads), vec![vec![0, 1, 2, 3, 4, 5]]);

        let mut mixed = reads.clone();
        mixed.extend([write("/w/x"), write("/w/y"), write("/w/z")]);
        assert_eq!(plan_waves(&mixed), vec![(0..9).collect::<Vec<_>>()]);

        assert_eq!(
            plan_waves(&[write("/w/a"), write("/w/a")]),
            vec![vec![0], vec![1]]
        );
        assert_eq!(
            plan_waves(&[read("/w/a"), exec(), read("/w/b")]),
            vec![vec![0], vec![1], vec![2]]
        );
        assert_eq!(
            plan_waves(&[write("/w/a"), read("/w/b"), write("/w/a")]),
            vec![vec![0, 1], vec![2]]
        );
        assert!(plan_waves(&[]).is_empty());
    }
}
