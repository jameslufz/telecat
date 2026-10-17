//! Virtualized data/result grid. Database writes remain the caller's responsibility.
pub mod model;
pub mod rows;
pub use rows::RowStore;
pub mod theme;
mod view;
pub use model::*;
pub use view::{Copy, DataGrid, FrameStats, Paste};
