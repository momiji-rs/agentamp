//! The glyphs the window draws controls with. Nerd Font icons by default,
//! as Omarchy's terminal font has them; `AGENTAMP_ICONS=plain` keeps to
//! characters every monospace font draws.

#[derive(Debug, PartialEq, Eq)]
pub struct Icons {
    pub play: &'static str,
    pub pause: &'static str,
    pub previous: &'static str,
    pub next: &'static str,
    /// The rounded ends of the play button, or nothing.
    pub cap_left: &'static str,
    pub cap_right: &'static str,
    pub volume_off: &'static str,
    pub volume_low: &'static str,
    pub volume_high: &'static str,
}

pub const NERD: Icons = Icons {
    play: "\u{f04b}",
    pause: "\u{f04c}",
    previous: "\u{f048}",
    next: "\u{f051}",
    cap_left: "\u{e0b6}",
    cap_right: "\u{e0b4}",
    volume_off: "\u{f026}",
    volume_low: "\u{f027}",
    volume_high: "\u{f028}",
};

pub const PLAIN: Icons = Icons {
    play: "▶",
    pause: "▌▌",
    previous: "|◀",
    next: "▶|",
    cap_left: "",
    cap_right: "",
    volume_off: "vol",
    volume_low: "vol",
    volume_high: "vol",
};

pub fn from_env() -> &'static Icons {
    match std::env::var("AGENTAMP_ICONS").as_deref() {
        Ok("plain") => &PLAIN,
        _ => &NERD,
    }
}
