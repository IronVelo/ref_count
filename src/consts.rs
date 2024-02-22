//! Lock State Constants
/// Bit denoting that references are blocked
pub const REF_BLOCKED: u32 = 1;
/// Bit denoting that references are allowed
pub const REF_ALLOWED: u32 = 0;
/// Mask out lock state bit
pub const REF_COUNT_MASK: u32 = !1;
/// Shift to get Ref Count
pub const REF_COUNT_SHIFT: u32 = 1;
/// Ref Increment Quantity
pub const REF_INCR: u32 = 1 << REF_COUNT_SHIFT;

// IMPORTANT NOTE REGARDING POTENTIAL UB IF CHANGED:
//
// The potential output of this mask should be a single bit describing if blocked or allowed.
// If this is changed the State::from_raw associated function MUST BE UPDATED to correspond with the
// changes.
/// Mask out the reader count
pub const STATE_MASK: u32 = 1;