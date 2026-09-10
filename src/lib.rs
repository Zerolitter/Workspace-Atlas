//! Workspace Atlas library entrypoint.
//!
//! Local-first catalogue, provider, query, context, and lifecycle services.

pub mod broker;
pub mod catalogue;
pub mod cli;
pub mod config;
pub mod context_application;
pub mod context_ir;
pub mod context_metrics;
pub mod context_route;
pub mod context_yield;
pub mod discovery;
pub mod error;
pub mod generation;
pub mod generation_delta;
pub mod hashing;
pub mod ids;
pub mod lifecycle;
pub mod mcp_adapter;
pub mod migrations;
pub mod paths;
pub mod project_scope;
pub mod provider_contract;
pub mod provider_persistence;
pub mod provider_runtime;
pub mod providers;
pub mod query;
pub mod query_metrics;
pub mod resolution;
pub mod scip_decoder;
pub mod scip_mapping;
pub mod semantic_reconcile;
pub mod serving;
pub mod task_compiler;
pub mod task_session;
pub mod temporal;
pub mod workspace;
