# ADR 0004: Module system — Rust-first, lifecycle hooks

- Status: Accepted
- Date: 2026-08-02
- Deciders: Project maintainer

## Context

The project requires dynamic modules that integrate with request lifecycle
stages without creating uncontrolled coupling between modules and core.

## Decision

- **Pipeline model.** Modules declare which lifecycle `Phase`s they hook:
  `Connection`, `RequestHeaders`, `Route`, `Access`, `Content`,
  `ResponseHeaders`, `Log`. Modules are assembled into a per-`server`/
  `location` pipeline at config-build time; a module may `Continue`,
  `ShortCircuit`, or `Error`.
- **Coupling boundary.** Modules receive `&mut RequestContext` (immutable
  request + scoped mutable response). They never touch reactor internals,
  connection slabs, or worker state directly.
- **Static modules first.** Core subsystems (access, rate limiting, cache,
  compression, security headers) are compiled-in modules in the pipeline.
- **Dynamic modules (Rust-first).** Loaded as `cdylib`s via `libloading`
  through a small, documented C ABI shim (`extern "C" fn aegis_module_init`).
  Module ABI stability is guaranteed within a release train and documented in
  `docs/modules.md`. A future C ABI is possible behind the same shim but is
  not the primary target.
- **Safety.** Dynamic modules are treated as part of the trusted server (like
  Nginx modules); they can still only interact through `RequestContext`, so
  their blast radius is bounded.

## Consequences

- Adding a subsystem (e.g. a new access-checker or cache policy) is a config +
  one module; core stays stable.
- The pipeline ordering rules are config-driven and documented, keeping
  behavior predictable and testable.
- Dynamic loading is Phase 22; the trait/phase design ships earlier so static
  modules use the same machinery.
