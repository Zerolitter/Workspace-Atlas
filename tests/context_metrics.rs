use workspace_atlas::context_ir::{
    ContextUseEvent, ContextUseEventType, ObservationSource, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::context_metrics::{
    derive_context_metrics, CanonicalEvidenceId, CanonicalEvidenceKind, ContextMetricEvidence,
    ContextMetricInvalidity, EvidenceClass, MetricUnit, ObservedEvidence, SuppliedEvidence,
    MAX_CONTEXT_METRIC_EVIDENCE,
};
use workspace_atlas::hashing::content_hash_of_bytes;
use workspace_atlas::task_session::context_metric_evidence_from_events;

fn evidence(kind: CanonicalEvidenceKind, id: &str) -> CanonicalEvidenceId {
    CanonicalEvidenceId::entity(kind, id).unwrap()
}

fn event(
    event_id: &str,
    event_type: ContextUseEventType,
    observation_source: ObservationSource,
    entity_id: Option<&str>,
    details: serde_json::Value,
) -> ContextUseEvent {
    ContextUseEvent {
        schema_version: CONTEXT_SCHEMA_VERSION.to_string(),
        event_id: event_id.to_string(),
        task_session_id: "task-1".to_string(),
        context_id: None,
        item_id: None,
        entity_id: entity_id.map(str::to_string),
        event_type,
        observation_source,
        occurred_at: "2026-09-02T00:00:00.000Z".to_string(),
        bytes: None,
        details,
    }
}

#[test]
fn derives_exact_canonical_sets_and_transparent_fractions() {
    let file = evidence(CanonicalEvidenceKind::File, "src/a.rs");
    let symbol = evidence(CanonicalEvidenceKind::Symbol, "symbol:answer");
    let relationship = CanonicalEvidenceId::relationship("rel:answer->caller").unwrap();
    let expanded = evidence(CanonicalEvidenceKind::File, "src/extra.rs");
    let changed = evidence(CanonicalEvidenceKind::File, "src/changed.rs");

    let metrics = derive_context_metrics(&ContextMetricEvidence {
        supplied: vec![
            SuppliedEvidence::new(file.clone(), 100),
            SuppliedEvidence::new(symbol.clone(), 300),
            SuppliedEvidence::new(relationship.clone(), 0),
        ],
        reads: vec![
            ObservedEvidence::new(file.clone()),
            ObservedEvidence::new(file.clone()),
            ObservedEvidence::new(expanded.clone()),
        ],
        reported: vec![symbol.clone(), expanded.clone()],
        changed: vec![changed.clone()],
    })
    .unwrap();

    assert_eq!(
        metrics.supplied.into_iter().collect::<Vec<_>>(),
        vec![file.clone(), symbol.clone(), relationship]
    );
    assert_eq!(
        metrics.observed.into_iter().collect::<Vec<_>>(),
        vec![file.clone(), expanded.clone()]
    );
    assert_eq!(
        metrics.reported.into_iter().collect::<Vec<_>>(),
        vec![expanded.clone(), symbol]
    );
    assert_eq!(
        metrics.changed.into_iter().collect::<Vec<_>>(),
        vec![changed]
    );
    assert_eq!(
        metrics.expanded.into_iter().collect::<Vec<_>>(),
        vec![expanded]
    );
    assert_eq!(
        metrics.rediscovered.into_iter().collect::<Vec<_>>(),
        vec![file]
    );

    assert_eq!(metrics.context_precision_observed.numerator, 1);
    assert_eq!(metrics.context_precision_observed.denominator, 3);
    assert_eq!(metrics.context_precision_observed.unit, MetricUnit::Items);
    assert_eq!(
        metrics.context_precision_observed.evidence_class,
        EvidenceClass::AtlasObserved
    );
    assert_eq!(metrics.context_precision_observed.invalidity, None);

    assert_eq!(metrics.context_precision_reported.numerator, 1);
    assert_eq!(metrics.context_precision_reported.denominator, 3);
    assert_eq!(
        metrics.context_precision_reported.evidence_class,
        EvidenceClass::ReportedUse
    );

    assert_eq!(metrics.context_expansion_count.numerator, 1);
    assert_eq!(metrics.context_expansion_count.denominator, 1);
    assert_eq!(metrics.context_expansion_count.unit, MetricUnit::Items);

    assert_eq!(metrics.rediscovery_rate.numerator, 2);
    assert_eq!(metrics.rediscovery_rate.denominator, 3);
    assert_eq!(metrics.rediscovery_rate.unit, MetricUnit::Requests);

    assert_eq!(metrics.source_efficiency_observed.numerator, 100);
    assert_eq!(metrics.source_efficiency_observed.denominator, 400);
    assert_eq!(metrics.source_efficiency_observed.unit, MetricUnit::Bytes);
}

#[test]
fn observed_reads_never_become_reported_reasoning_claims() {
    let item = evidence(CanonicalEvidenceKind::File, "src/read-only.rs");
    let metrics = derive_context_metrics(&ContextMetricEvidence {
        supplied: vec![SuppliedEvidence::new(item.clone(), 64)],
        reads: vec![ObservedEvidence::new(item.clone())],
        reported: Vec::new(),
        changed: Vec::new(),
    })
    .unwrap();

    assert_eq!(metrics.observed.into_iter().collect::<Vec<_>>(), vec![item]);
    assert!(metrics.reported.is_empty());
    assert_eq!(metrics.context_precision_reported.numerator, 0);
    assert_eq!(metrics.context_precision_reported.denominator, 1);
    assert_eq!(
        metrics.context_precision_reported.evidence_class,
        EvidenceClass::ReportedUse
    );
}

#[test]
fn zero_denominators_are_typed_undefined_values() {
    let metrics = derive_context_metrics(&ContextMetricEvidence::default()).unwrap();

    for fraction in [
        metrics.context_precision_observed,
        metrics.context_precision_reported,
        metrics.rediscovery_rate,
        metrics.source_efficiency_observed,
    ] {
        assert_eq!(fraction.denominator, 0);
        assert_eq!(
            fraction.invalidity,
            Some(ContextMetricInvalidity::ZeroDenominator)
        );
    }
    assert_eq!(metrics.context_expansion_count.denominator, 1);
    assert_eq!(metrics.context_expansion_count.invalidity, None);
}

#[test]
fn conflicting_or_unbounded_evidence_fails_with_typed_invalidity() {
    let item = evidence(CanonicalEvidenceKind::File, "src/a.rs");
    let conflict = derive_context_metrics(&ContextMetricEvidence {
        supplied: vec![
            SuppliedEvidence::new(item.clone(), 10),
            SuppliedEvidence::new(item.clone(), 11),
        ],
        ..ContextMetricEvidence::default()
    });
    assert_eq!(
        conflict.unwrap_err(),
        ContextMetricInvalidity::ConflictingSuppliedBytes { identity: item }
    );

    let too_many = (0..=MAX_CONTEXT_METRIC_EVIDENCE)
        .map(|index| {
            ObservedEvidence::new(evidence(
                CanonicalEvidenceKind::File,
                &format!("src/{index}.rs"),
            ))
        })
        .collect();
    let unbounded = derive_context_metrics(&ContextMetricEvidence {
        reads: too_many,
        ..ContextMetricEvidence::default()
    });
    assert_eq!(
        unbounded.unwrap_err(),
        ContextMetricInvalidity::CapacityExceeded {
            limit: MAX_CONTEXT_METRIC_EVIDENCE,
            actual: MAX_CONTEXT_METRIC_EVIDENCE + 1,
        }
    );
}

#[test]
fn task_session_transport_preserves_observed_reported_and_changed_classes() {
    let events = vec![
        event(
            "supplied",
            ContextUseEventType::ContextSupplied,
            ObservationSource::AtlasObserved,
            Some("src/a.rs"),
            serde_json::json!({"entity_kind": "file", "source_bytes": 80}),
        ),
        event(
            "read",
            ContextUseEventType::ExactSourceRequested,
            ObservationSource::AtlasObserved,
            None,
            serde_json::json!({
                "request_kind": "source",
                "path_hash": content_hash_of_bytes(b"src/a.rs"),
                "status": "verified",
            }),
        ),
        event(
            "reported",
            ContextUseEventType::ReasoningUseReported,
            ObservationSource::AgentReported,
            Some("src/a.rs"),
            serde_json::json!({}),
        ),
        event(
            "changed",
            ContextUseEventType::ArtifactChanged,
            ObservationSource::AtlasObserved,
            Some("src/changed.rs"),
            serde_json::json!({}),
        ),
        event(
            "untrusted-reported-class",
            ContextUseEventType::ReasoningUseReported,
            ObservationSource::AtlasObserved,
            Some("src/not-reported.rs"),
            serde_json::json!({}),
        ),
        event(
            "untrusted-read-class",
            ContextUseEventType::EntityQueried,
            ObservationSource::AgentReported,
            Some("src/not-observed.rs"),
            serde_json::json!({"status": "succeeded"}),
        ),
    ];

    let normalized = context_metric_evidence_from_events(&events).unwrap();
    let metrics = derive_context_metrics(&normalized).unwrap();

    assert_eq!(metrics.context_precision_observed.numerator, 1);
    assert_eq!(metrics.context_precision_reported.numerator, 1);
    assert_eq!(
        metrics.changed.into_iter().collect::<Vec<_>>(),
        vec![evidence(CanonicalEvidenceKind::File, "src/changed.rs")]
    );
    assert!(!metrics.reported.contains(&evidence(
        CanonicalEvidenceKind::File,
        "src/not-reported.rs"
    )));
    assert!(!metrics.observed.contains(&evidence(
        CanonicalEvidenceKind::File,
        "src/not-observed.rs"
    )));
}
