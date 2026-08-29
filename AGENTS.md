# AGENTS.md

# AGENTS.md

## Project

This project is a minimal AI coding agent written from scratch in Rust.

The long-term goal is to build a **Nix-native Agent Runtime** rather than simply clone an existing coding agent.

The project is intentionally built from first principles.

## Core Principles

### 1. Keep the core small

Do not add features unless they are required by the current task.

Prefer a small, understandable implementation over a feature-rich framework.

### 2. Agent Core must be independent of the runtime

The architecture should separate:

```text
Model
  ↓
Agent Core
  ↓
Tools
  ↓
Runtime
  ↓
Operating System
```

The Agent Core must not directly depend on:

- `std::process::Command`
- shell commands
- Nix
- macOS-specific APIs
- Linux-specific APIs

These belong in the Runtime layer.

### 3. Tools are typed capabilities

Tools should be explicit and strongly typed.

Prefer:

```rust
enum Tool {
    Read(Read),
    Write(Write),
    Search(Search),
    Exec(Exec),
}
```

Avoid exposing an unrestricted shell as the primary interface.

The goal is to eventually model tools as capabilities that the Runtime can restrict.

### 4. Runtime abstraction

The Agent should interact with the outside world through a Runtime abstraction.

The initial implementation may use:

```text
LocalRuntime
```

Later implementations may include:

```text
NixRuntime
SandboxRuntime
VMRuntime
```

Do not couple the Agent Core to any specific Runtime implementation.

### 5. Nix is part of the long-term architecture

Nix is not merely a package manager for this project.

The long-term goal is to use Nix to describe and provide:

- dependencies
- development environments
- execution environments
- reproducibility
- capabilities
- sandbox boundaries

However, **do not introduce Nix-specific complexity before the basic Agent works.**

The first working implementation should be able to run with `LocalRuntime`.

## Development Order

Follow this order unless there is a strong reason not to:

1. LLM provider abstraction
2. Messages
3. Agent loop
4. Tool abstraction
5. `read`
6. `write`
7. `search`
8. `exec`
9. `LocalRuntime`
10. `NixRuntime`
11. capabilities
12. sandboxing
13. sessions
14. subagents

Do not implement later stages prematurely.

## Rust Guidelines

Prefer:

- small structs
- enums for finite states
- traits for stable boundaries
- explicit error types
- `Result` instead of panics
- immutable data where practical
- simple async code

Avoid introducing large frameworks unless they solve a demonstrated problem.

Do not create abstractions merely because they might be useful later.

## Dependencies

Keep dependencies minimal.

Before adding a crate, ask:

1. Is it actually necessary?
2. Can the standard library solve this cleanly?
3. Does it introduce a large abstraction for a small problem?
4. Does it make the architecture harder to understand?

Prefer mature, small dependencies.

## Testing

Every new core behavior should have a test.

Prioritize testing:

- Agent state transitions
- tool parsing
- tool execution
- runtime behavior
- error handling

Tests should not require an external LLM unless the test specifically verifies model integration.

Prefer deterministic tests.

## Nix

The project uses Nix Flakes for the development environment.

Use:

```bash
nix develop
```

to enter the development environment.

Do not install project dependencies globally.

The project should eventually be reproducible from its Nix configuration.

## CLI

The CLI should remain simple.

Prefer:

```text
mypi
```

for interactive mode and:

```text
mypi "task"
```

for one-shot tasks.

Do not add CLI options unless they correspond to an actual feature.

## Agent Behavior

When working on a task:

1. Inspect the existing code.
2. Understand the current architecture.
3. Make the smallest change that solves the task.
4. Run relevant tests.
5. Run formatting and static checks.
6. Do not refactor unrelated code.

Do not rewrite working code merely to make it look different.

## Commands

Before considering a change complete, run the appropriate checks:

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy
```

When Nix integration exists, also verify the relevant Nix commands.

## Important Constraint

This project is an experiment in designing an Agent Runtime.

Do not optimize for feature parity with Pi, OpenCode, Claude Code, or other existing agents.

When choosing between:

```text
"copy an existing Agent's behavior"
```

and:

```text
"design a cleaner primitive"
```

prefer the cleaner primitive when it does not unnecessarily complicate the implementation.

## Current Goal

The immediate goal is deliberately small:

```text
LLM
 ↓
Agent Loop
 ↓
read / write / search / exec
 ↓
LocalRuntime
```

Get this working before implementing Nix-specific runtime features.

## Tool Usage

- **Search:** Use Pi's built-in `grep` / `find` tools. For multiple OR patterns, use `multi_grep` in a single call. If Bash search is unavoidable, use `rg`, never `grep`.
- **Read:** After locating a file or match, use `read` with `offset` / `limit` to inspect only the relevant region. For known files outside the workspace, use `read` directly.

### Context Discipline

- Prefer bounded, structured tool output over raw shell output.
- Do not use recursive `grep` / `find` through the workspace.
- Do not dump entire files when only a relevant section is needed.
- Avoid commands whose output can grow with repository size.
- Treat context as a limited resource: retrieve only what is necessary to make the next decision.
