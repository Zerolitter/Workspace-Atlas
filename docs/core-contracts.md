# Core operation contracts

This is the normative operation and state-transition specification for Stage 1
of the [post-v2 strengthening roadmap](post-v2-strengthening-roadmap.md).
It makes the existing v2 public guarantees explicit; it introduces no new
schema, public command, lifecycle version, or optional intelligence dependency.
The [context compiler ADR](adr/023-context-compiler-contract.md) remains the
authority and compatibility boundary.

## Operation guarantees

The roadmap names describe logical operations, not new public function names.

| Logical operation | Guarantee | Implementation boundary |
| --- | --- | --- |
| `reconcile_workspace()` | Reconciliation constructs generation-bound file and provider evidence and applies activation policy before replacing the active generation; unsuccessful construction preserves the previous active generation. | `discovery::reconcile` |
| `activate_generation()` | Only a candidate belonging to the workspace may be committed, made active, and used to reconcile eligible legacy sessions, and those changes either commit together or leave their previous state unchanged. | `generation::activate_candidate` |
| `accept_provider_result()` | Only output that passes runtime outcome, confinement, integrity, decoding, mapping, and input-identity checks may contribute provider facts; observed cancellation cannot become successful evidence because the process also exited zero. | `semantic_reconcile::run_one_plan`, `provider_runtime`, `provider_persistence` |
| `build_serving_projection()` | Serving construction derives a generation- and policy-addressed projection without modifying Truth, and an incomplete build never exposes a partial ready projection. | `serving::build_serving_generation_with_policy` |
| `resolve_relationship()` | Resolution uses deterministic evidence precedence, preserves unresolved or ambiguous state and its bounded candidates, and never promotes display-name similarity alone into verified truth. | `resolution::resolve_relationship` |
| `compile_context_ir()` | Compilation returns a bounded, generation-bound evidence packet with explicit uncertainty and omissions, and change-mode source references must pass live verification before they become usable. | `task_compiler::compile_context_ir`, `compile_deep_context_ir_v2` and the application sealing boundary |
| `governor_run()` | The Governor executes only negotiated and permitted deterministic acquisition routes, may succeed with zero Atlas context, and blocks a changed or unavailable generation rather than combining evidence across generations. | `context_application::run_catalogue_governor`, `context_route::execute_context_route` |
| `verify_exact_source()` | Exact-source verification checks confined live bytes against the requested committed generation and revision, rejects observed source or generation drift, and materializes bytes only within explicit authority and bounds. | `query::verify_exact_source_at_generation` and application source sealing |
| `complete_task()` | Legacy completion requires a reconciled session and matching active committed end-generation and source tree, commits the terminal state with its delta, outcome and artifact events atomically, and accepts only identical replay. | `task_session::complete_legacy_task_session`, `complete_task_session` |
| `abandon_task()` | Legacy abandonment atomically terminates only an active or reconciled session, accepts identical replay, rejects conflicting replay, and changes no workspace source or generation truth. | `task_session::abandon_legacy_task_session` |

These are operation-boundary guarantees. Reconciliation owns provider policy
and construction checks; calling the low-level activation function alone is
not a substitute for reconciliation. A complete runtime exit is likewise not
proof that an artifact passes decoding, mapping, or input-identity checks.

## Generation state machine

Persisted generation states form this closed vocabulary:

```text
candidate -> committed
candidate -> failed
candidate -> abandoned
```

`committed`, `failed`, and `abandoned` have no outgoing transitions through the
generation operations. Failed and abandoned generations cannot be activated.
An unsuccessful operation must not alter the active pointer or a terminal
generation's fields. A new attempt creates a new candidate and sequence number.

The roadmap's conceptual sequence is:

```text
candidate -> prerequisites validated -> committed and active
```

`validated` is a prerequisite, not a persisted state. `active` is the
workspace's pointer to a committed generation, not an additional generation
state. Candidate commitment, active-pointer replacement, and reconciliation of
all and only older-generation active legacy sessions share one immediate
transaction. Readers observe a complete previous or new committed generation;
an already-open read snapshot may continue observing the previous generation.
Requests that require the current generation must detect drift separately.

Construction can persist candidate evidence before activation. A failure may
leave a candidate for diagnosis and explicit crash recovery; it must never
present that candidate as current. Recovery may mark an in-progress candidate
abandoned, then build a new candidate. The preceding committed generation
remains authoritative throughout failure or recovery.

## Provider acceptance and cancellation

The acceptance sequence is runtime observation, output ownership and integrity
validation, decode, map, input/revision binding, then core-owned persistence.
Providers receive no catalogue handle or Truth activation authority.

| Outcome | Required provider | Optional provider |
| --- | --- | --- |
| Complete | May proceed through remaining checks | May proceed through remaining checks |
| Partial | May proceed with visible degraded coverage | May proceed with visible degraded coverage |
| Failed, unavailable, timed out, cancelled, output rejected, decode failed, mapping failed, unsupported | Blocks candidate activation | Preserves explicit degraded coverage |

Cancellation is monotonic for an invocation: once requested, the caller does
not clear the flag while that invocation is running. Cancellation observed at
process outcome selection or after bounded cleanup/capture takes precedence
over a successful exit. Polling or containment errors still fail closed.
Cancellation after the runtime returns does not retroactively cancel an
accepted invocation. Current reconciliation does not expose an asynchronous
cancellation API; this contract does not invent one or imply persistence is
independently cancellable.

## Legacy task state machine

Persisted legacy task states and legal transitions are:

| From | Legal next states |
| --- | --- |
| Created | ContextCompiled, Failed |
| ContextCompiled | Active, Failed |
| Active | Reconciled, Abandoned, Failed |
| Reconciled | Completed, Abandoned, Failed |
| Completed | None |
| Abandoned | None |
| Failed | None |

There are no implicit self-transitions or transitions out of a terminal state.
Identical public completion/abandonment replay returns the existing terminal
result; it is not a new state transition. Conflicting replay fails without
changing the result. Completion uses the transactional completion operation,
not the internal state-transition primitive alone.

Completed DIRECT `direct_none` acquisition can activate a valid bound legacy
session without fabricating an IR. Completed or useful partial LIGHT/DEEP
acquisition may activate it. Blocked, interrupted, error, and no-useful-payload
acquisition must not activate it. An interrupted transient V2 request does not
create durable V2 lifecycle state; a prior completed payload can remain useful
only with its recorded generation, provenance, and limitations.

## Serving, source and contract validity

Serving is derived and rebuildable. Its generation, policy and input identity
must match before use. A new generation supersedes an old projection; consumed
projection corruption permits at most one bounded Truth fallback using the
same request-wide deadline. Fallback must preserve canonical evidence and may
not reconstruct or scan the repository. Projection readiness alone proves
neither task sufficiency nor current live-source validity.

A Context IR or source lease describes verified inputs at a particular
generation and revision. Source can change after compilation. A consumer must
verify exact live source again before change use; an earlier packet, successful
route, cached projection, or historical record cannot authorize stale bytes.
Drift, uncertainty and bounded omissions stay explicit.

Package, catalogue, provider, MCP, planner, capability, route, execution,
operation and Context IR versions remain separate contracts. Unsupported
versions fail closed at their own negotiation or decoding boundary. An
explicit legacy IR read cannot be relabelled as V2 sufficiency. All 19 v2 MCP
interfaces retain their existing authority; this document adds none.

## Qualification map

The named tests are executable guards for the specification, not alternate
sources of legal state transitions. Run them on the exact candidate tree.

| Failure scenario | Test coverage |
| --- | --- |
| Failure during reconciliation | `semantic_reconcile::tests::required_semantic_provider_failure_leaves_previous_generation_active`; `task_outcome_delta::generation_activation_fault_rolls_back_pointer_candidate_and_session` |
| Provider finishes after cancellation | `provider_runtime::tests::cancellation_wins_when_successful_exit_is_already_observed`; `cancellation_at_capture_completion_preserves_diagnostics_without_success`; `cancellation_flag_terminates_the_process_tree` |
| Generation changes during a request | `compiler_lifecycle::changed_generation_blocks_until_the_caller_resubmits_a_new_decision`; `exact_source_compiler::generation_drift_fails_closed_before_any_source_is_sealed` |
| Serving projection becomes stale | `serving_readiness::status_tracks_absent_ready_failed_policy_drift_and_generation_supersession` and consumed-corruption fallback tests |
| Source changes after compilation | `exact_source_compiler::source_drift_seals_blocked_stale_ir_without_durable_telemetry`; `generation_delta::tests::evidence_lease_verifies_against_live_source_and_auto_invalidates_on_change` |
| Interrupted task lifecycle | `compiler_lifecycle::deep_interruption_retains_only_the_prior_completed_light_payload`; `task_session::tests::every_legacy_transition_matches_the_contract`; `compiler_surface_parity` completion/abandonment replay tests |
| Concurrent readers and writers | `generation::tests::reader_snapshot_remains_committed_across_activation`; `task_outcome_delta::generation_activation_acquires_immediate_transaction_before_verification`; catalogue writer-lease tests |
| Unsupported contract versions | `context_route_decision::capability_discovery_and_negotiation_are_privacy_bounded_and_fail_closed`; `task_compiler_v20::old_and_new_readers_are_explicit_and_fail_closed` |

`generation::tests::every_generation_transition_matches_the_contract` guards
the complete operation/state matrix, including terminal rejection with no
mutation. This is deterministic state-machine qualification, not a benchmark,
performance claim, model experiment, or proof of arbitrary hostile SQL safety.
