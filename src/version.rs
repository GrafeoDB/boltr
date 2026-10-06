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
    let (slots, _) = proposals.as_chunks::<4>();
    for slot in slots {
        if is_placeholder(slot) {
            continue;
        }

        // Check if any of our supported versions falls within the proposed range.
        for &(sup_major, sup_minor) in &SUPPORTED_VERSIONS {
            if slot_covers(slot, sup_major, sup_minor) {
                return Some((sup_major, sup_minor));
            }
        }
    }
    None
}

/// Returns true if `major.minor` is covered by one of the 4 proposals.
///
/// Clients use this to check that the version a server picked is one they
/// actually offered (a non-Bolt peer, such as an HTTP server, answers the
/// handshake with arbitrary bytes).
pub fn proposals_cover(proposals: &[u8; 16], major: u8, minor: u8) -> bool {
    let (slots, _) = proposals.as_chunks::<4>();
    slots
        .iter()
        .any(|slot| !is_placeholder(slot) && slot_covers(slot, major, minor))
}

/// An all-zero version slot is an unused proposal.
fn is_placeholder(slot: &[u8; 4]) -> bool {
    slot[2] == 0 && slot[3] == 0
}

/// Whether a proposal slot (`[reserved, range, minor, major]`) covers `major.minor`.
fn slot_covers(slot: &[u8; 4], major: u8, minor: u8) -> bool {
    let [_, range, slot_minor, slot_major] = *slot;
    slot_major == major && minor <= slot_minor && minor >= slot_minor.saturating_sub(range)
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

    #[test]
    fn negotiate_each_supported_minor_exactly() {
        for minor in 1..=4u8 {
            let mut proposals = [0u8; 16];
            proposals[2] = minor;
            proposals[3] = 5;
            assert_eq!(negotiate_version(&proposals), Some((5, minor)));
        }
    }

    #[test]
    fn negotiate_rejects_unsupported_minors() {
        // 5.0 has no LOGON and is not supported; 5.5 and later without a
        // range reaching back to 5.4 are not supported either.
        for minor in [0u8, 5, 8, 255] {
            let mut proposals = [0u8; 16];
            proposals[2] = minor;
            proposals[3] = 5;
            assert_eq!(negotiate_version(&proposals), None, "5.{minor}");
        }
    }

    #[test]
    fn negotiate_range_reaching_below_zero() {
        // 5.2 with range 200 saturates at 5.0 and still covers 5.2..5.1.
        let mut proposals = [0u8; 16];
        proposals[1] = 200;
        proposals[2] = 2;
        proposals[3] = 5;
        assert_eq!(negotiate_version(&proposals), Some((5, 2)));
    }

    #[test]
    fn negotiate_skips_manifest_style_and_garbage_slots() {
        // Slot 0: Bolt 5.7+ handshake manifest marker (0x000001FF).
        // Slot 1: random bytes. Slot 2: 5.8 with range 8.
        let proposals = [
            0x00, 0x00, 0x01, 0xFF, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x08, 0x08, 0x05, 0, 0, 0, 0,
        ];
        assert_eq!(negotiate_version(&proposals), Some((5, 4)));
    }

    #[test]
    fn negotiate_typical_driver_proposals() {
        // Neo4j 5.x drivers: 5.4..5.0, 4.4..4.2, 4.1, 3.0.
        let proposals = [0, 4, 4, 5, 0, 2, 4, 4, 0, 0, 1, 4, 0, 0, 0, 3];
        assert_eq!(negotiate_version(&proposals), Some((5, 4)));
        // A 4.x-only driver gets no version.
        let proposals = [0, 2, 4, 4, 0, 0, 1, 4, 0, 0, 0, 3, 0, 0, 0, 0];
        assert_eq!(negotiate_version(&proposals), None);
    }

    #[test]
    fn negotiate_every_garbage_proposal_without_panicking() {
        // Exhaustive over one slot (the other three empty): never panics and
        // only ever returns a supported version that the slot covers.
        for range in [0u8, 1, 3, 255] {
            for minor in 0..=255u8 {
                for major in 0..=255u8 {
                    let mut proposals = [0u8; 16];
                    proposals[1] = range;
                    proposals[2] = minor;
                    proposals[3] = major;
                    if let Some((ma, mi)) = negotiate_version(&proposals) {
                        assert!(SUPPORTED_VERSIONS.contains(&(ma, mi)));
                        assert!(proposals_cover(&proposals, ma, mi));
                    }
                }
            }
        }
    }

    #[test]
    fn proposals_cover_checks_every_slot() {
        let mut proposals = [0u8; 16];
        proposals[1] = 3;
        proposals[2] = 4;
        proposals[3] = 5;
        assert!(proposals_cover(&proposals, 5, 4));
        assert!(proposals_cover(&proposals, 5, 1));
        assert!(!proposals_cover(&proposals, 5, 0));
        assert!(!proposals_cover(&proposals, 4, 4));
        // "HTTP" read back as a version is not covered.
        assert!(!proposals_cover(&proposals, b'P', b'T'));
        // Placeholders never cover 0.0.
        assert!(!proposals_cover(&[0u8; 16], 0, 0));
    }
}
