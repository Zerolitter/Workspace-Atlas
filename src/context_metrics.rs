//! Pure, bounded derivation of Context Yield working sets and fractions.
//!
//! Persistence adapters normalize typed task-session evidence into
//! [`ContextMetricEvidence`]. This module performs only deterministic set
//! algebra; observed reads and reported-use claims remain distinct inputs and
//! outputs by construction.

use std::collections::{BTreeMap, BTreeSet};

/// Hard bound for raw evidence records accepted by one derivation.
pub const MAX_CONTEXT_METRIC_EVIDENCE: usize = 4096;
/// Hard semantic-counter bounds for one transient context execution.
pub const MAX_CONTEXT_EXECUTION_ATLAS_CALLS: u64 = 1_024;
pub const MAX_CONTEXT_EXECUTION_RECORDS: u64 = 100_000;
pub const MAX_CONTEXT_EXECUTION_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS: u64 = 4_000_000;
pub const MAX_CONTEXT_EXECUTION_WORK_UNITS: u64 = 10_000_000;

/// Transport-neutral semantic counters. They are observations only and never
/// participate in route or Context IR identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextExecutionCounters {
    pub atlas_calls: u64,
    pub records: u64,
    pub source_bytes: u64,
    pub estimated_tokens: u64,
    pub work_units: u64,
}

impl ContextExecutionCounters {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.atlas_calls > MAX_CONTEXT_EXECUTION_ATLAS_CALLS {
            return Err("execution Atlas-call counter exceeds its bound");
        }
        if self.records > MAX_CONTEXT_EXECUTION_RECORDS {
            return Err("execution record counter exceeds its bound");
        }
        if self.source_bytes > MAX_CONTEXT_EXECUTION_SOURCE_BYTES {
            return Err("execution source-byte counter exceeds its bound");
        }
        if self.estimated_tokens > MAX_CONTEXT_EXECUTION_ESTIMATED_TOKENS {
            return Err("execution estimated-token counter exceeds its bound");
        }
        if self.work_units > MAX_CONTEXT_EXECUTION_WORK_UNITS {
            return Err("execution work-unit counter exceeds its bound");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CanonicalEvidenceKind {
    File,
    Symbol,
    Config,
    Test,
    Query,
    Document,
    External,
    Relationship,
    Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalRange {
    pub start: u64,
    pub end: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalEvidenceId {
    kind: CanonicalEvidenceKind,
    id: String,
    range: Option<CanonicalRange>,
}

impl CanonicalEvidenceId {
    pub fn entity(
        kind: CanonicalEvidenceKind,
        id: impl Into<String>,
    ) -> Result<Self, ContextMetricInvalidity> {
        if kind == CanonicalEvidenceKind::Range {
            return Err(ContextMetricInvalidity::InvalidIdentity);
        }
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ContextMetricInvalidity::InvalidIdentity);
        }
        Ok(Self {
            kind,
            id,
            range: None,
        })
    }

    pub fn kind(&self) -> CanonicalEvidenceKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn canonical_range(&self) -> Option<CanonicalRange> {
        self.range
    }

    pub fn relationship(id: impl Into<String>) -> Result<Self, ContextMetricInvalidity> {
        Self::entity(CanonicalEvidenceKind::Relationship, id)
    }

    pub fn range(
        path_identity: impl Into<String>,
        start: u64,
        end: u64,
    ) -> Result<Self, ContextMetricInvalidity> {
        if start > end {
            return Err(ContextMetricInvalidity::InvalidRange { start, end });
        }
        let id = path_identity.into();
        if id.trim().is_empty() {
            return Err(ContextMetricInvalidity::InvalidIdentity);
        }
        Ok(Self {
            kind: CanonicalEvidenceKind::Range,
            id,
            range: Some(CanonicalRange { start, end }),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppliedEvidence {
    pub identity: CanonicalEvidenceId,
    pub exact_source_bytes: u64,
}

impl SuppliedEvidence {
    pub fn new(identity: CanonicalEvidenceId, exact_source_bytes: u64) -> Self {
        Self {
            identity,
            exact_source_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedEvidence {
    pub identity: CanonicalEvidenceId,
}

impl ObservedEvidence {
    pub fn new(identity: CanonicalEvidenceId) -> Self {
        Self { identity }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextMetricEvidence {
    pub supplied: Vec<SuppliedEvidence>,
    pub reads: Vec<ObservedEvidence>,
    pub reported: Vec<CanonicalEvidenceId>,
    pub changed: Vec<CanonicalEvidenceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    Items,
    Requests,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceClass {
    AtlasObserved,
    ReportedUse,
    ObservedAndReported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMetricInvalidity {
    ZeroDenominator,
    InvalidIdentity,
    InvalidRange {
        start: u64,
        end: u64,
    },
    InvalidSourceBytes {
        identity: CanonicalEvidenceId,
    },
    MissingEvidenceIdentity {
        event_id: String,
    },
    UnsupportedEntityKind {
        event_id: String,
        entity_kind: String,
    },
    ConflictingSuppliedBytes {
        identity: CanonicalEvidenceId,
    },
    CapacityExceeded {
        limit: usize,
        actual: usize,
    },
    ArithmeticOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricFraction {
    pub numerator: u64,
    pub denominator: u64,
    pub unit: MetricUnit,
    pub evidence_class: EvidenceClass,
    pub invalidity: Option<ContextMetricInvalidity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMetrics {
    pub supplied: BTreeSet<CanonicalEvidenceId>,
    pub observed: BTreeSet<CanonicalEvidenceId>,
    pub reported: BTreeSet<CanonicalEvidenceId>,
    pub changed: BTreeSet<CanonicalEvidenceId>,
    pub expanded: BTreeSet<CanonicalEvidenceId>,
    pub rediscovered: BTreeSet<CanonicalEvidenceId>,
    pub context_precision_observed: MetricFraction,
    pub context_precision_reported: MetricFraction,
    pub context_expansion_count: MetricFraction,
    pub rediscovery_rate: MetricFraction,
    pub source_efficiency_observed: MetricFraction,
}

fn metric(
    numerator: u64,
    denominator: u64,
    unit: MetricUnit,
    evidence_class: EvidenceClass,
) -> MetricFraction {
    MetricFraction {
        numerator,
        denominator,
        unit,
        evidence_class,
        invalidity: (denominator == 0).then_some(ContextMetricInvalidity::ZeroDenominator),
    }
}

fn usize_to_u64(value: usize) -> Result<u64, ContextMetricInvalidity> {
    u64::try_from(value).map_err(|_| ContextMetricInvalidity::ArithmeticOverflow)
}

/// Derive canonical Context Yield sets and transparent measures.
///
/// Duplicate identities are canonicalized. Repeated reads remain visible in
/// the rediscovery numerator and request denominator. A read can populate only
/// `observed`; it can never populate `reported` or create a reasoning claim.
pub fn derive_context_metrics(
    evidence: &ContextMetricEvidence,
) -> Result<ContextMetrics, ContextMetricInvalidity> {
    let actual = evidence
        .supplied
        .len()
        .checked_add(evidence.reads.len())
        .and_then(|count| count.checked_add(evidence.reported.len()))
        .and_then(|count| count.checked_add(evidence.changed.len()))
        .ok_or(ContextMetricInvalidity::ArithmeticOverflow)?;
    if actual > MAX_CONTEXT_METRIC_EVIDENCE {
        return Err(ContextMetricInvalidity::CapacityExceeded {
            limit: MAX_CONTEXT_METRIC_EVIDENCE,
            actual,
        });
    }

    let mut supplied_bytes = BTreeMap::<CanonicalEvidenceId, u64>::new();
    for item in &evidence.supplied {
        match supplied_bytes.get(&item.identity) {
            Some(bytes) if *bytes != item.exact_source_bytes => {
                return Err(ContextMetricInvalidity::ConflictingSuppliedBytes {
                    identity: item.identity.clone(),
                });
            }
            Some(_) => {}
            None => {
                supplied_bytes.insert(item.identity.clone(), item.exact_source_bytes);
            }
        }
    }

    let supplied = supplied_bytes.keys().cloned().collect::<BTreeSet<_>>();
    let observed = evidence
        .reads
        .iter()
        .map(|item| item.identity.clone())
        .collect::<BTreeSet<_>>();
    let reported = evidence.reported.iter().cloned().collect::<BTreeSet<_>>();
    let changed = evidence.changed.iter().cloned().collect::<BTreeSet<_>>();
    let expanded = observed
        .union(&reported)
        .filter(|identity| !supplied.contains(*identity))
        .cloned()
        .collect::<BTreeSet<_>>();
    let rediscovered = observed
        .intersection(&supplied)
        .cloned()
        .collect::<BTreeSet<_>>();

    let observed_supplied = observed.intersection(&supplied).count();
    let reported_supplied = reported.intersection(&supplied).count();
    let rediscovery_requests = evidence
        .reads
        .iter()
        .filter(|read| supplied.contains(&read.identity))
        .count();

    let supplied_source_bytes = supplied_bytes.values().try_fold(0_u64, |total, bytes| {
        total
            .checked_add(*bytes)
            .ok_or(ContextMetricInvalidity::ArithmeticOverflow)
    })?;
    let observed_supplied_source_bytes = supplied_bytes
        .iter()
        .filter(|(identity, _)| observed.contains(*identity))
        .try_fold(0_u64, |total, (_, bytes)| {
            total
                .checked_add(*bytes)
                .ok_or(ContextMetricInvalidity::ArithmeticOverflow)
        })?;

    Ok(ContextMetrics {
        context_precision_observed: metric(
            usize_to_u64(observed_supplied)?,
            usize_to_u64(supplied.len())?,
            MetricUnit::Items,
            EvidenceClass::AtlasObserved,
        ),
        context_precision_reported: metric(
            usize_to_u64(reported_supplied)?,
            usize_to_u64(supplied.len())?,
            MetricUnit::Items,
            EvidenceClass::ReportedUse,
        ),
        context_expansion_count: metric(
            usize_to_u64(expanded.len())?,
            1,
            MetricUnit::Items,
            EvidenceClass::ObservedAndReported,
        ),
        rediscovery_rate: metric(
            usize_to_u64(rediscovery_requests)?,
            usize_to_u64(evidence.reads.len())?,
            MetricUnit::Requests,
            EvidenceClass::AtlasObserved,
        ),
        source_efficiency_observed: metric(
            observed_supplied_source_bytes,
            supplied_source_bytes,
            MetricUnit::Bytes,
            EvidenceClass::AtlasObserved,
        ),
        supplied,
        observed,
        reported,
        changed,
        expanded,
        rediscovered,
    })
}
