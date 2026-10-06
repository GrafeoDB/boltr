//! Typed write counters from a result summary.

use crate::types::{BoltDict, BoltValue};

/// Write counters reported in a result summary (Neo4j's `stats` metadata,
/// which drivers expose as `summary.counters`).
///
/// A server includes a `stats` dictionary in the final PULL (or DISCARD)
/// SUCCESS of a statement that changed data. It holds only the non-zero
/// counters, under Neo4j's key names (`nodes-created`, `properties-set`,
/// ...), plus `contains-updates`. Missing keys read as zero, so the summary
/// of a read-only statement yields all zeros.
///
/// ```
/// use boltr::client::Counters;
/// use boltr::types::{BoltDict, BoltValue};
///
/// let stats = BoltDict::from([
///     ("nodes-created".to_string(), BoltValue::Integer(2)),
///     ("properties-set".to_string(), BoltValue::Integer(3)),
///     ("contains-updates".to_string(), BoltValue::Boolean(true)),
/// ]);
/// let summary = BoltDict::from([("stats".to_string(), BoltValue::Dict(stats))]);
///
/// let counters = Counters::from_summary(&summary);
/// assert_eq!(counters.nodes_created, 2);
/// assert_eq!(counters.properties_set, 3);
/// assert_eq!(counters.relationships_created, 0);
/// assert!(counters.contains_updates());
///
/// assert!(!Counters::from_summary(&BoltDict::new()).contains_updates());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counters {
    pub nodes_created: u64,
    pub nodes_deleted: u64,
    pub relationships_created: u64,
    pub relationships_deleted: u64,
    pub properties_set: u64,
    pub labels_added: u64,
    pub labels_removed: u64,
    pub indexes_added: u64,
    pub indexes_removed: u64,
    pub constraints_added: u64,
    pub constraints_removed: u64,
    pub system_updates: u64,
    /// Explicit `contains-updates` flag, when the server sent one.
    contains_updates_flag: Option<bool>,
    /// Explicit `contains-system-updates` flag, when the server sent one.
    contains_system_updates_flag: Option<bool>,
}

impl Counters {
    /// Reads the counters from the `stats` entry of a result summary
    /// (the metadata of the final PULL or DISCARD SUCCESS).
    #[must_use]
    pub fn from_summary(summary: &BoltDict) -> Self {
        match summary.get("stats") {
            Some(BoltValue::Dict(stats)) => Self::from_stats(stats),
            _ => Self::default(),
        }
    }

    /// Reads the counters from a `stats` dictionary itself.
    #[must_use]
    pub fn from_stats(stats: &BoltDict) -> Self {
        let count = |key: &str| match stats.get(key) {
            Some(BoltValue::Integer(value)) => u64::try_from(*value).unwrap_or(0),
            _ => 0,
        };
        let flag = |key: &str| match stats.get(key) {
            Some(BoltValue::Boolean(value)) => Some(*value),
            _ => None,
        };
        Self {
            nodes_created: count("nodes-created"),
            nodes_deleted: count("nodes-deleted"),
            relationships_created: count("relationships-created"),
            relationships_deleted: count("relationships-deleted"),
            properties_set: count("properties-set"),
            labels_added: count("labels-added"),
            labels_removed: count("labels-removed"),
            indexes_added: count("indexes-added"),
            indexes_removed: count("indexes-removed"),
            constraints_added: count("constraints-added"),
            constraints_removed: count("constraints-removed"),
            system_updates: count("system-updates"),
            contains_updates_flag: flag("contains-updates"),
            contains_system_updates_flag: flag("contains-system-updates"),
        }
    }

    /// Whether the statement changed data: the server's `contains-updates`
    /// flag if it sent one, otherwise whether any data counter is non-zero.
    #[must_use]
    pub fn contains_updates(&self) -> bool {
        self.contains_updates_flag.unwrap_or_else(|| {
            [
                self.nodes_created,
                self.nodes_deleted,
                self.relationships_created,
                self.relationships_deleted,
                self.properties_set,
                self.labels_added,
                self.labels_removed,
                self.indexes_added,
                self.indexes_removed,
                self.constraints_added,
                self.constraints_removed,
            ]
            .iter()
            .any(|&count| count > 0)
        })
    }

    /// Whether the statement changed the system database: the server's
    /// `contains-system-updates` flag if it sent one, otherwise whether
    /// `system_updates` is non-zero.
    #[must_use]
    pub fn contains_system_updates(&self) -> bool {
        self.contains_system_updates_flag
            .unwrap_or(self.system_updates > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_with(stats: &[(&str, BoltValue)]) -> BoltDict {
        let stats: BoltDict = stats
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        BoltDict::from([
            ("stats".to_string(), BoltValue::Dict(stats)),
            ("type".to_string(), BoltValue::String("w".into())),
        ])
    }

    #[test]
    fn reads_every_neo4j_counter() {
        let keys = [
            "nodes-created",
            "nodes-deleted",
            "relationships-created",
            "relationships-deleted",
            "properties-set",
            "labels-added",
            "labels-removed",
            "indexes-added",
            "indexes-removed",
            "constraints-added",
            "constraints-removed",
            "system-updates",
        ];
        let stats: Vec<(&str, BoltValue)> = keys
            .iter()
            .zip(1..)
            .map(|(k, v)| (*k, BoltValue::Integer(v)))
            .collect();
        let c = Counters::from_summary(&summary_with(&stats));
        assert_eq!(
            [
                c.nodes_created,
                c.nodes_deleted,
                c.relationships_created,
                c.relationships_deleted,
                c.properties_set,
                c.labels_added,
                c.labels_removed,
                c.indexes_added,
                c.indexes_removed,
                c.constraints_added,
                c.constraints_removed,
                c.system_updates,
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
        assert!(c.contains_updates());
        assert!(c.contains_system_updates());
    }

    #[test]
    fn missing_stats_reads_as_zero() {
        let c = Counters::from_summary(&BoltDict::new());
        assert_eq!(c, Counters::default());
        assert!(!c.contains_updates());
        assert!(!c.contains_system_updates());

        // `stats` of the wrong type is ignored too.
        let summary = BoltDict::from([("stats".to_string(), BoltValue::Integer(3))]);
        assert_eq!(Counters::from_summary(&summary), Counters::default());
    }

    #[test]
    fn invalid_counter_values_read_as_zero() {
        let c = Counters::from_summary(&summary_with(&[
            ("nodes-created", BoltValue::Integer(-5)),
            ("nodes-deleted", BoltValue::String("3".into())),
            ("properties-set", BoltValue::Float(2.0)),
        ]));
        assert_eq!(c.nodes_created, 0);
        assert_eq!(c.nodes_deleted, 0);
        assert_eq!(c.properties_set, 0);
        assert!(!c.contains_updates());
    }

    #[test]
    fn explicit_flags_take_precedence() {
        // grafeo-server style: only the non-zero counters plus the flag.
        let c = Counters::from_summary(&summary_with(&[
            ("labels-added", BoltValue::Integer(1)),
            ("contains-updates", BoltValue::Boolean(true)),
        ]));
        assert_eq!(c.labels_added, 1);
        assert!(c.contains_updates());

        let c = Counters::from_summary(&summary_with(&[
            ("nodes-created", BoltValue::Integer(1)),
            ("contains-updates", BoltValue::Boolean(false)),
            ("contains-system-updates", BoltValue::Boolean(true)),
        ]));
        assert!(!c.contains_updates());
        assert!(c.contains_system_updates());
    }

    #[test]
    fn derives_contains_updates_without_flag() {
        let c = Counters::from_summary(&summary_with(&[(
            "relationships-deleted",
            BoltValue::Integer(2),
        )]));
        assert!(c.contains_updates());
        assert!(!c.contains_system_updates());
    }
}
