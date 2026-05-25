//! Backend selection for subagent spawns.
//!
//! Selection order (highest priority first):
//! 1. an explicit `isolation` value on the request,
//! 2. the `OPENHARNESSRS_TEAMMATE_MODE` env override (matching the existing
//!    `OPENHARNESSRS_*` convention),
//! 3. the default ([`SubagentIsolation::InProcess`]).
//!
//! Phase 0 only resolves `InProcess` to a real backend ([`InProcessBackend`]);
//! `Subprocess` and `Worktree` return [`SubagentError::BackendUnimplemented`].
//! The registry, selection logic, and env override exist now so Phases 2-3 can
//! slot real backends in behind the same API.

use std::sync::Arc;

use oh_swarm::{Backend, InProcessBackend};
use oh_types::subagent::{SubagentError, SubagentIsolation};

/// Name of the env var that overrides the default teammate/subagent backend.
pub const TEAMMATE_MODE_ENV: &str = "OPENHARNESSRS_TEAMMATE_MODE";

/// Selects and constructs the swarm [`Backend`] for a subagent spawn.
pub struct BackendRegistry;

impl BackendRegistry {
    pub fn new() -> Self {
        Self
    }

    /// Resolve the effective isolation mode for a request, applying the
    /// env override and default. `explicit` is the value carried on the
    /// [`SpawnRequest`](oh_types::subagent::SpawnRequest); pass the request's
    /// `isolation` field. When it is the default `InProcess`, the env override
    /// (if any) takes effect.
    pub fn resolve_mode(&self, explicit: SubagentIsolation) -> SubagentIsolation {
        // An explicitly non-default request value wins outright.
        if explicit != SubagentIsolation::InProcess {
            return explicit;
        }
        match std::env::var(TEAMMATE_MODE_ENV).ok().as_deref() {
            Some("subprocess") => SubagentIsolation::Subprocess,
            Some("worktree") => SubagentIsolation::Worktree,
            Some("in_process") | Some("inprocess") => SubagentIsolation::InProcess,
            _ => SubagentIsolation::InProcess,
        }
    }

    /// Construct the backend for the given (already-resolved) mode.
    ///
    /// Only `InProcess` is wired in Phase 0.
    pub fn backend_for(&self, mode: SubagentIsolation) -> Result<Arc<dyn Backend>, SubagentError> {
        match mode {
            SubagentIsolation::InProcess => {
                let team_root = oh_config::get_tasks_dir().join("teammates");
                Ok(Arc::new(InProcessBackend::new(team_root)))
            }
            SubagentIsolation::Subprocess => Err(SubagentError::BackendUnimplemented(
                "subprocess".to_string(),
            )),
            SubagentIsolation::Worktree => {
                Err(SubagentError::BackendUnimplemented("worktree".to_string()))
            }
        }
    }

    /// Convenience: resolve the mode for `explicit` and construct its backend.
    pub fn select(&self, explicit: SubagentIsolation) -> Result<Arc<dyn Backend>, SubagentError> {
        self.backend_for(self.resolve_mode(explicit))
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate a process-global env var; they are grouped and the
    // var is cleared at the end of each so they don't race within a thread.

    #[test]
    fn test_explicit_non_default_wins_over_env() {
        let reg = BackendRegistry::new();
        // Even with no env, an explicit Worktree request resolves to Worktree.
        assert_eq!(
            reg.resolve_mode(SubagentIsolation::Worktree),
            SubagentIsolation::Worktree
        );
    }

    #[test]
    fn test_default_without_env_is_in_process() {
        std::env::remove_var(TEAMMATE_MODE_ENV);
        let reg = BackendRegistry::new();
        assert_eq!(
            reg.resolve_mode(SubagentIsolation::InProcess),
            SubagentIsolation::InProcess
        );
    }

    #[test]
    fn test_env_override_applies_to_default_request() {
        let reg = BackendRegistry::new();
        std::env::set_var(TEAMMATE_MODE_ENV, "subprocess");
        assert_eq!(
            reg.resolve_mode(SubagentIsolation::InProcess),
            SubagentIsolation::Subprocess
        );
        std::env::set_var(TEAMMATE_MODE_ENV, "worktree");
        assert_eq!(
            reg.resolve_mode(SubagentIsolation::InProcess),
            SubagentIsolation::Worktree
        );
        std::env::remove_var(TEAMMATE_MODE_ENV);
    }

    #[test]
    fn test_backend_for_in_process_is_real() {
        let reg = BackendRegistry::new();
        assert!(reg.backend_for(SubagentIsolation::InProcess).is_ok());
    }

    #[test]
    fn test_backend_for_subprocess_and_worktree_unimplemented() {
        let reg = BackendRegistry::new();
        assert!(matches!(
            reg.backend_for(SubagentIsolation::Subprocess),
            Err(SubagentError::BackendUnimplemented(_))
        ));
        assert!(matches!(
            reg.backend_for(SubagentIsolation::Worktree),
            Err(SubagentError::BackendUnimplemented(_))
        ));
    }
}
