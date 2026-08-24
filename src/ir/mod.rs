//! The textual intermediate representation.
//!
//! v0 grammar (one statement per line, `#` starts a comment):
//!
//! ```text
//! extern <slug>: <Type> match (uuid|iname|title) "<value>"
//! block  <slug>: <Type> ["Title"] [{ <Param> = <value> … }]
//! wire   <slug>.<Port> -> <slug>.<Port>
//! set    <slug>.<Port> = <value>
//! ```
//!
//! Slugs are `[a-z][a-z0-9_]*` and project-unique. References are always
//! slugs — never UUIDs, never titles. `match iname` is preferred over
//! `match title` for built-in objects because display titles are
//! locale-volatile (a config save can rename all built-ins to the writing
//! system's language); `match uuid` pins exactly.

mod ast;
mod compile;
mod decompile;
mod parser;

pub use ast::{BlockDecl, ExternDecl, Item, MatchSpec, Module, PortRef, SetDecl, WireDecl};
pub use compile::{CompileOptions, compile};
pub use decompile::{DecompileOptions, DecompileReport, decompile, slugify};
