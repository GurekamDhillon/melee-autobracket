//! Standard bracket seeding order.

/// Seed placed at each first-round position of a bracket of `size` (a power of two).
/// Positions `2k` and `2k + 1` meet in first-round set `k`; the better seed comes first,
/// and the top two seeds can only meet in the final.
pub fn seed_positions(size: u32) -> Vec<u32> {
    debug_assert!(size.is_power_of_two(), "bracket size must be a power of two");
    let mut order = vec![0];
    let mut len = 1;
    while len < size {
        len *= 2;
        order = order.iter().flat_map(|&s| [s, len - 1 - s]).collect();
    }
    order
}
