//! Low-allocation compiler query instrumentation.
//!
//! Only the closed numeric and identity columns already present in
//! `query_stage_metric` are persisted. Task text, source, paths, and arbitrary
//! details never enter this module.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rusqlite::{params, Connection};

use crate::error::Result;

static METRIC_ORDINAL: AtomicU64 = AtomicU64::new(1);

pub trait MetricClock {
    fn now_micros(&self) -> u64;
}

pub struct SystemMetricClock {
    started: Instant,
}

impl SystemMetricClock {
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl MetricClock for SystemMetricClock {
    fn now_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }
}

#[derive(Debug, Clone, Copy)]
pub enum QueryStage {
    SeedResolution,
    ServingLookup,
    GraphExpansion,
    Ranking,
    SourceVerify,
    SourceRead,
    Packing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryMetrics {
    pub seed_resolution_us: i64,
    pub serving_lookup_us: i64,
    pub graph_expansion_us: i64,
    pub ranking_us: i64,
    pub source_verify_us: i64,
    pub source_read_us: i64,
    pub packing_us: i64,
    pub candidate_count: i64,
    pub expanded_edge_count: i64,
    pub source_bytes_read: i64,
    pub returned_records: i64,
    pub returned_estimated_tokens: i64,
    pub cache_hits: i64,
    pub cache_misses: i64,
    pub truncated: bool,
    /// Repository-scale work is prohibited on the compiler hot path. These
    /// counters are deliberately diagnostic-only and are never persisted.
    pub workspace_walks: i64,
    pub provider_spawns: i64,
    pub parses: i64,
    pub resolution_calls: i64,
}

impl QueryMetrics {
    pub fn total_us(&self) -> i64 {
        self.seed_resolution_us
            .saturating_add(self.serving_lookup_us)
            .saturating_add(self.graph_expansion_us)
            .saturating_add(self.ranking_us)
            .saturating_add(self.source_verify_us)
            .saturating_add(self.source_read_us)
            .saturating_add(self.packing_us)
    }

    pub fn prohibited_hot_path_work(&self) -> i64 {
        self.workspace_walks
            .saturating_add(self.provider_spawns)
            .saturating_add(self.parses)
            .saturating_add(self.resolution_calls)
    }
}

pub struct QueryMetricIdentity<'a> {
    pub workspace_id: &'a str,
    pub generation_id: &'a str,
    pub task_session_id: &'a str,
    pub request_id: &'a str,
    pub serving_fallback: bool,
}

pub struct QueryMetricCollector<'a> {
    clock: &'a dyn MetricClock,
    started_us: u64,
    checkpoint_us: u64,
    metrics: QueryMetrics,
}

impl<'a> QueryMetricCollector<'a> {
    pub fn new(clock: &'a dyn MetricClock) -> Self {
        let now = clock.now_micros();
        Self {
            clock,
            started_us: now,
            checkpoint_us: now,
            metrics: QueryMetrics::default(),
        }
    }

    pub fn checkpoint(&mut self, stage: QueryStage) {
        let now = self.clock.now_micros();
        let elapsed = now.saturating_sub(self.checkpoint_us).min(i64::MAX as u64) as i64;
        self.checkpoint_us = now;
        let target = match stage {
            QueryStage::SeedResolution => &mut self.metrics.seed_resolution_us,
            QueryStage::ServingLookup => &mut self.metrics.serving_lookup_us,
            QueryStage::GraphExpansion => &mut self.metrics.graph_expansion_us,
            QueryStage::Ranking => &mut self.metrics.ranking_us,
            QueryStage::SourceVerify => &mut self.metrics.source_verify_us,
            QueryStage::SourceRead => &mut self.metrics.source_read_us,
            QueryStage::Packing => &mut self.metrics.packing_us,
        };
        *target = target.saturating_add(elapsed);
    }

    pub fn elapsed_us(&self) -> u64 {
        self.clock.now_micros().saturating_sub(self.started_us)
    }

    pub fn exceeds_millis(&self, limit_ms: i64) -> bool {
        self.elapsed_us() > (limit_ms as u64).saturating_mul(1_000)
    }

    pub fn metrics_mut(&mut self) -> &mut QueryMetrics {
        &mut self.metrics
    }

    pub fn metrics(&self) -> &QueryMetrics {
        &self.metrics
    }

    pub fn persist(
        &self,
        connection: &Connection,
        identity: &QueryMetricIdentity<'_>,
        operation: &'static str,
    ) -> Result<()> {
        let ordinal = METRIC_ORDINAL.fetch_add(1, Ordering::Relaxed).to_string();
        let created_at = crate::migrations::iso8601_now();
        let process_id = std::process::id().to_string();
        let metric_id = crate::resolution::deterministic_id(
            "qmetric",
            &[
                identity.workspace_id,
                identity.generation_id,
                identity.task_session_id,
                identity.request_id,
                operation,
                &created_at,
                &process_id,
                &ordinal,
            ],
        );
        connection.execute(
            "INSERT INTO query_stage_metric (
                metric_id, workspace_id, generation_id, task_session_id, context_id,
                request_id, operation, serving_fallback, seed_resolution_us,
                serving_lookup_us, graph_expansion_us, ranking_us, source_verify_us,
                source_read_us, packing_us, candidate_count, expanded_edge_count,
                source_bytes_read, returned_records, returned_estimated_tokens,
                cache_hits, cache_misses, truncated, created_at
             ) VALUES (
                ?1,?2,?3,(SELECT task_session_id FROM task_session WHERE task_session_id=?4),
                NULL,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                ?20,?21,?22,?23
             )",
            params![
                metric_id,
                identity.workspace_id,
                identity.generation_id,
                identity.task_session_id,
                identity.request_id,
                operation,
                i64::from(identity.serving_fallback),
                self.metrics.seed_resolution_us,
                self.metrics.serving_lookup_us,
                self.metrics.graph_expansion_us,
                self.metrics.ranking_us,
                self.metrics.source_verify_us,
                self.metrics.source_read_us,
                self.metrics.packing_us,
                self.metrics.candidate_count,
                self.metrics.expanded_edge_count,
                self.metrics.source_bytes_read,
                self.metrics.returned_records,
                self.metrics.returned_estimated_tokens,
                self.metrics.cache_hits,
                self.metrics.cache_misses,
                i64::from(self.metrics.truncated),
                created_at,
            ],
        )?;
        Ok(())
    }
}
