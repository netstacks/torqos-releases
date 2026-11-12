//! TorqOS Settings (ADR-045 2): torqcli in a COSMIC window. No privilege and no way of its own to change the
//! system: the person's own account, and torqcli's own sessions (`torq::app::Console`), so the
//! window does what the command line does and says it in the same words. Every action it offers is a
//! torqcli command the person could type, and it shows which.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

mod window;

fn main() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default().size_limits(cosmic::iced::Limits::NONE.min_width(480.0).min_height(360.0));
    // `torq-settings <page>`: where the panel's applet sends a person.
    cosmic::app::run::<window::Settings>(settings, std::env::args().nth(1))
}
