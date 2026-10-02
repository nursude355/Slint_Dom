//! Lightweight, web-native rendering for a focused subset of Slint.
//!
//! [`include_ui!`] reads a `.slint` file at compile time and generates a Rust
//! component which creates ordinary DOM nodes. The parser is never linked into
//! the WebAssembly binary.
//!
//! The crate is intended for small Rust/WASM control panels, telemetry
//! displays, and embedded-devices' web UIs that can run complex code (such as
//! generating statistics or charts, coded in Rust) in the browser, and want
//! to use Slint for the UI layout and styling without doing the full official
//! slint compilation, using DOM generated Widgets rather than WASM rendered
//! Widgets and by that drastically reduces the size of the WASM binary.
//!
//! A WASM example is provided that shows how to use the crate to create a simple
//! line chart and some widgets with a Slint UI.
//!
//! A Server example is provided that creates a web server application that
//! the WASM example can connect to and shows sending unsolicited messages
//! in both directions via WebSockets using an RPC protocol.
//! The server example can be compiled as well for a desktop development computer
//! as for a Raspberry Pi Pico W with the W5500 ethernet module and run on the device.
//!
mod callback;
mod chart;
mod dom;
mod property;
#[cfg(feature = "rpc")]
pub mod rpc;

pub use callback::Callback;
pub use chart::LineChart;
pub use dom::{DomBuilder, EventBinding};
pub use property::{Property, Subscription, UpdateCycle};
pub use slint_dom_macros::include_ui;

#[doc(hidden)]
pub mod __private {
    pub use wasm_bindgen::JsValue;
    pub use web_sys::Element;
}
