//! Core of cal-tui: vdir storage, iCalendar model, recurrence expansion,
//! queries and edits. Contains no UI code. See DESIGN.md.

pub mod config;
pub mod ical;
pub mod recur;
pub mod store;
pub mod time;

pub use config::Config;
pub use store::{Calendar, Instance, Kind, Store, Todo, Warning};
pub use time::Moment;

/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
