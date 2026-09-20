use crate::tui::app::DataState;

/// Data provider for the TUI.
pub trait DataSource {
    fn load(&mut self) -> DataState;
    /// Load a fresh snapshot only when the underlying store changed.
    fn load_if_changed(&mut self) -> Option<DataState> {
        Some(self.load())
    }
    /// Load all stopped agents (no time cutoff).
    fn load_all_stopped(&mut self) -> Vec<crate::tui::model::Agent>;
    /// Last backend/data-source error, if any.
    fn last_error(&self) -> Option<String> {
        None
    }
    /// Set the default timeline event limit (overridden by HCOM_TUI_TIMELINE_LIMIT env).
    fn set_timeline_limit(&mut self, _limit: usize) {}
    /// Reconcile local instances whose tracked process has died since the last
    /// pass, returning how many were cleaned up. Fixture/remote sources have
    /// no PIDs to check, so the default is a no-op.
    fn reconcile_dead_instances(&mut self) -> anyhow::Result<usize> {
        Ok(0)
    }
}

/// Create the DB-backed DataSource.
pub fn create_data_source() -> Box<dyn DataSource> {
    Box::new(crate::tui::db::DbDataSource::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::Agent;

    /// Stand-in for a fixture/mock DataSource (no DB behind it). Only
    /// `load`/`load_all_stopped` are implemented; everything else, including
    /// `reconcile_dead_instances`, must come from the trait default.
    struct FixtureSource;
    impl DataSource for FixtureSource {
        fn load(&mut self) -> DataState {
            DataState::empty()
        }
        fn load_all_stopped(&mut self) -> Vec<Agent> {
            vec![]
        }
    }

    #[test]
    fn fixture_source_reconcile_is_a_noop() {
        let mut source = FixtureSource;
        // No PID to check and nothing to mutate — the default trait impl
        // must just report zero, never touch process state or panic.
        assert_eq!(source.reconcile_dead_instances().unwrap(), 0);
    }
}
