//! Expertise profiles (docs/tasks/EXPERTISE.md): what good work looks like for the person and
//! for each project, kept as short entries that are pushed into a worker's or reviewer's brief,
//! can be searched, and grow from what happened in runs. Only the person accepts a new entry;
//! agents propose candidates and flag them.
//!
//! The pieces live here so the work on them does not collide:
//!
//! - [`push`]: the entries a worker is shown when a task is handed out.
//! - `tools` (added later): `expertise_search` / `expertise_get`, and the results
//!   `task_accept` / `task_drop` record.
//! - [`review`]: the reviewer's checklist, and candidate creation from a `fail` verdict.
//! - [`candidates`]: candidates from rework and first-try merges, the lead's tools, the
//!   `run_finish` count.
//! - [`maintain`]: pure functions for the budget, staleness, conflicts, the card and the
//!   Markdown export.

pub mod candidates;
pub mod maintain;
pub mod push;
pub mod review;
