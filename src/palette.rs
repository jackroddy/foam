//! The eight colours of the screen, one per concept, so a colour
//! says the same thing wherever it appears. Emphasis and quiet are
//! bold and dim, not colours.

use ratatui::style::Color;

/// Something named: an issue id, a memory's slug, the prefix, a
/// branch and its commit.
pub const REF: Color = Color::Cyan;
/// Open, and what opens an issue: create, reopen, unclaim.
pub const OPEN: Color = Color::Green;
/// Claimed, the claim, and the lease.
pub const CLAIMED: Color = Color::Yellow;
/// Deferred.
pub const DEFERRED: Color = Color::Blue;
/// What the cursor is on: the chosen box, the focused pane's title,
/// the view in use.
pub const FOCUS: Color = Color::LightBlue;
/// An agent, and what agents keep: the memories.
pub const AGENT: Color = Color::Magenta;
/// What needs a look: P0 and P1, a memory that may no longer hold.
pub const ALARM: Color = Color::Red;
/// The background under the chosen row and under code.
pub const SHADE: Color = Color::DarkGray;

// ---
// Not concepts of their own: what a badge's text is printed in,
// and two tints the badges use where bold cannot tell P0 from P1
// or lift P2 off the shade.

/// The text on a badge.
pub const INK: Color = Color::Black;
/// A softer alarm: the P1 badge.
pub const ALARM_SOFT: Color = Color::LightRed;
/// A step above the shade: the P2 badge.
pub const QUIET: Color = Color::Gray;
