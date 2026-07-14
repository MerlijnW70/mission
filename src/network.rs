//! The **network** layer of the Mission browser.
//!
//! Constitutional boundary: network fetches bytes, nothing more. It must never reach into
//! the renderer (it cannot draw) — an architectural boundary of the design.
//! This is a small, fully-tested stub: a single primitive to hold a green baseline until the
//! real fetch/transport code lands.

/// Whether an HTTP status code denotes success (the `2xx` class).
///
/// The half-open `200..300` range has clear edges the tests pin: either bound
/// can shift and `..` can flip to `..=`. The tests below pin the boundaries (199/200/299/300)
/// so no such mutant can survive.
pub fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_2xx_band_is_success() {
        assert!(is_success(200)); // lower edge — kills the `200`→`201` bound shift
        assert!(is_success(299)); // upper interior
        assert!(!is_success(300)); // upper edge — kills `300`→`301` and `..`→`..=`
        assert!(!is_success(199)); // below the band
        assert!(!is_success(0));
    }
}
