//! uci-core: UCI parsing, the transcript format and the shim <-> viewer
//! stream format.
//!
//! Shared by the shim (`uci-shim`) and the viewer (`uci-viewer`). It has no
//! dependencies on purpose: the shim sits inside someone else's GUI and must
//! stay tiny.

pub mod cmdline;
pub mod json;
pub mod parse;
pub mod paths;
pub mod session;
pub mod transcript;
pub mod view;

pub use cmdline::split_args;
pub use parse::{
    parse_line, GoParams, Info, Message, OptionDecl, OptionKind, Score, ScoreBound, ScoreKind,
};
pub use session::SessionInfo;
pub use transcript::{Dir, Event};
pub use view::view_json;
