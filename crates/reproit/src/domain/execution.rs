//! Deterministic state and outcome rules for source-neutral reproduction runs.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionPhase {
    Validate,
    Reserve,
    Reset,
    Build,
    Seed,
    Launch,
    Readiness,
    Debug,
    Trigger,
    Observe,
    Retain,
    Cleanup,
}

impl ExecutionPhase {
    pub(crate) const ORDER: [Self; 12] = [
        Self::Validate,
        Self::Reserve,
        Self::Reset,
        Self::Build,
        Self::Seed,
        Self::Launch,
        Self::Readiness,
        Self::Debug,
        Self::Trigger,
        Self::Observe,
        Self::Retain,
        Self::Cleanup,
    ];
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PhaseStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PhaseRecord {
    pub(crate) phase: ExecutionPhase,
    pub(crate) status: PhaseStatus,
    pub(crate) detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransitionError {
    OutOfOrder,
    AlreadyFinished,
}

#[derive(Debug)]
pub(crate) struct ExecutionState {
    records: Vec<PhaseRecord>,
    next_index: usize,
    failed: bool,
}

impl ExecutionState {
    pub(crate) fn new() -> Self {
        Self {
            records: ExecutionPhase::ORDER
                .into_iter()
                .map(|phase| PhaseRecord {
                    phase,
                    status: PhaseStatus::Pending,
                    detail: String::new(),
                })
                .collect(),
            next_index: 0,
            failed: false,
        }
    }

    pub(crate) fn start(&mut self, phase: ExecutionPhase) -> Result<(), TransitionError> {
        if self.next_index >= self.records.len() {
            return Err(TransitionError::AlreadyFinished);
        }
        if self.records[self.next_index].phase != phase {
            return Err(TransitionError::OutOfOrder);
        }
        self.records[self.next_index].status = PhaseStatus::Running;
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        phase: ExecutionPhase,
        status: PhaseStatus,
        detail: impl Into<String>,
    ) -> Result<(), TransitionError> {
        if self.next_index >= self.records.len() {
            return Err(TransitionError::AlreadyFinished);
        }
        let record = &mut self.records[self.next_index];
        if record.phase != phase || record.status != PhaseStatus::Running {
            return Err(TransitionError::OutOfOrder);
        }
        if !matches!(
            status,
            PhaseStatus::Passed | PhaseStatus::Failed | PhaseStatus::Skipped
        ) {
            return Err(TransitionError::OutOfOrder);
        }
        record.status = status;
        record.detail = detail.into();
        self.failed |= status == PhaseStatus::Failed;
        self.next_index += 1;
        Ok(())
    }

    pub(crate) fn skip_until(
        &mut self,
        target: ExecutionPhase,
        detail: &str,
    ) -> Result<(), TransitionError> {
        while self.next_index < self.records.len() && self.records[self.next_index].phase < target {
            let phase = self.records[self.next_index].phase;
            self.start(phase)?;
            self.finish(phase, PhaseStatus::Skipped, detail)?;
        }
        Ok(())
    }

    pub(crate) fn fail_and_advance_to_cleanup(
        &mut self,
        phase: ExecutionPhase,
        detail: impl Into<String>,
    ) -> Result<(), TransitionError> {
        self.finish(phase, PhaseStatus::Failed, detail)?;
        self.skip_until(ExecutionPhase::Cleanup, "skipped after failure")
    }

    pub(crate) fn records(&self) -> &[PhaseRecord] {
        &self.records
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionVerdict {
    Reproduced,
    NotReproduced,
    Flaky,
    Stale,
    Incomplete,
    Unsupported,
    DifferentFailure,
    InfrastructureFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_cannot_run_out_of_order() {
        let mut state = ExecutionState::new();
        assert_eq!(
            state.start(ExecutionPhase::Build),
            Err(TransitionError::OutOfOrder)
        );
        state.start(ExecutionPhase::Validate).unwrap();
        state
            .finish(ExecutionPhase::Validate, PhaseStatus::Passed, "valid")
            .unwrap();
        assert_eq!(state.records()[0].status, PhaseStatus::Passed);
    }

    #[test]
    fn failure_still_requires_cleanup() {
        let mut state = ExecutionState::new();
        state.start(ExecutionPhase::Validate).unwrap();
        state
            .fail_and_advance_to_cleanup(ExecutionPhase::Validate, "bad digest")
            .unwrap();
        assert!(state.failed());
        assert_eq!(state.records().last().unwrap().status, PhaseStatus::Pending);
        state.start(ExecutionPhase::Cleanup).unwrap();
        state
            .finish(
                ExecutionPhase::Cleanup,
                PhaseStatus::Passed,
                "nothing owned",
            )
            .unwrap();
    }
}
