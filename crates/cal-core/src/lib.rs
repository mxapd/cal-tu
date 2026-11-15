//! Core of cal-tui: vdir storage, iCalendar model, recurrence expansion,
//! queries and edits. Contains no UI code. See DESIGN.md.

pub mod alarms;
pub mod config;
pub mod edit;
pub mod fsio;
pub mod ical;
pub mod layout;
pub mod quick;
pub mod recur;
pub mod store;
pub mod time;
pub mod vtimezone;

pub use alarms::Alarm;
pub use config::{Config, WeatherConfig};
pub use edit::{EditError, EventEdit, NewEvent, NewTodo, Scope, TodoEdit};
pub use store::{Calendar, Instance, Kind, Store, Todo, Warning};
pub use time::Moment;

/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
