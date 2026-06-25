//! The IRC engine: connection management, protocol handling, and the typed
//! events bridged to the frontend.

pub mod auth;
pub mod connection;
pub mod dcc;
pub mod event;
pub mod ircx;
pub mod manager;
pub mod state;
pub mod stream;

pub use manager::ConnectionManager;
