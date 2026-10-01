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
}

/// Create the DB-backed DataSource.
#[cfg(not(test))]
pub fn create_data_source() -> Box<dyn DataSource> {
    Box::new(crate::tui::db::DbDataSource::new())
}

/// Unit tests build `App` without holding the env lock, so a DB source would
/// open whatever `HCOM_DIR` another test currently owns and race that test's
/// first open (SQLITE_BUSY on Windows). Tests needing a DB set `app.source`.
#[cfg(test)]
pub fn create_data_source() -> Box<dyn DataSource> {
    Box::new(FixtureSource)
}

/// Stand-in for a fixture/mock DataSource (no DB behind it). Only
/// `load`/`load_all_stopped` are implemented; everything else must come from
/// the trait default.
#[cfg(test)]
struct FixtureSource;

#[cfg(test)]
impl DataSource for FixtureSource {
    fn load(&mut self) -> DataState {
        DataState::empty()
    }
    fn load_all_stopped(&mut self) -> Vec<crate::tui::model::Agent> {
        vec![]
    }
}
