//! Bolt protocol version negotiation.

/// Bolt magic preamble bytes.
pub const BOLT_MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];

/// Supported Bolt versions (major, minor) in preference order.
pub const SUPPORTED_VERSIONS: [(u8, u8); 4] = [
    (5, 4), // Primary target
    (5, 3),
    (5, 2),
    (5, 1), // Minimum (has LOGON/LOGOFF)
];

/// Parses the 4 client-proposed versions (16 bytes) and returns the best match.
///
/// Each proposal is a 4-byte big-endian value:
/// - byte 0: padding (reserved)
/// - byte 1: range (count of prior minor versions also accepted)
/// - byte 2: minor version
/// - byte 3: major version
///
/// Returns `None` if no supported version matches any proposal.
pub fn negotiate_version(proposals: &[u8; 16]) -> Option<(u8, u8)> {
    for chunk in proposals.chunks_exact(4) {
        let range = chunk[1];
        let minor = chunk[2];
        let major = chunk[3];

        if major == 0 && minor == 0 {
            // Placeholder (unused proposal slot).
            continue;
        }

        // Check if any of our supported versions falls within the proposed range.
        for &(sup_major, sup_minor) in &SUPPORTED_VERSIONS {
            if sup_major == major
                && sup_minor <= minor
                && sup_minor >= minor.saturating_sub(range)
            {
                return Some((sup_major, sup_minor));
            }
        }
    }
    None
}

/// Encodes a version as a 4-byte big-endian response.
pub fn encode_version(major: u8, minor: u8) -> [u8; 4] {
    [0, 0, minor, major]
}

/// The "no version" response sent when negotiation fails.
pub const NO_VERSION: [u8; 4] = [0, 0, 0, 0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_exact_match() {
        // Client proposes exactly 5.4.
        let mut proposals = [0u8; 16];
        proposals[2] = 4; // minor
        proposals[3] = 5; // major
        assert_eq!(negotiate_version(&proposals), Some((5, 4)));
    }

    #[test]
    fn negotiate_range_match() {
        // Client proposes 5.6 with range 3 (covers 5.6, 5.5, 5.4, 5.3).
        let mut proposals = [0u8; 16];
        proposals[1] = 3; // range
        proposals[2] = 6; // minor
        proposals[3] = 5; // major
        assert_eq!(negotiate_version(&proposals), Some((5, 4)));
    }

    #[test]
    fn negotiate_no_match() {
        // Client only supports 4.x.
        let mut proposals = [0u8; 16];
        proposals[2] = 4; // minor
        proposals[3] = 4; // major
        assert_eq!(negotiate_version(&proposals), None);
    }

    #[test]
    fn negotiate_second_proposal() {
        // First proposal is unsupported, second is 5.2.
        let mut proposals = [0u8; 16];
        // Slot 0: 6.0 (unsupported)
        proposals[2] = 0;
        proposals[3] = 6;
        // Slot 1: 5.2
        proposals[6] = 2; // minor
        proposals[7] = 5; // major
        assert_eq!(negotiate_version(&proposals), Some((5, 2)));
    }

    #[test]
    fn negotiate_all_zeros() {
        let proposals = [0u8; 16];
        assert_eq!(negotiate_version(&proposals), None);
    }

    #[test]
    fn encode_version_54() {
        assert_eq!(encode_version(5, 4), [0, 0, 4, 5]);
    }
}
