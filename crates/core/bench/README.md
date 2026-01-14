# Benchmarking rblib-core

## Overview

The `revm_regression` benchmarks compare checkpoint operations against equivalent direct REVM/BundleState operations to
quantify abstraction overhead.

### Scenarios

| Benchmark     | What it measures                                                   |
|---------------|--------------------------------------------------------------------|
| `apply_*`     | Transaction execution: checkpoint chain vs single mutable State    |
| `revert_*`    | Revert capability: checkpoint fork vs BundleState::revert_latest() |
| `traversal_*` | State lookup cost at varying checkpoint chain depths               |
| `forking`     | Cost of creating parallel branches from shared checkpoint          |

See [`benches/revm_regression/scenarios.rs`](../benches/revm_regression/scenarios.rs) for detailed
scenario definitions including workloads, approaches compared, and what each benchmark measures.

### Harnesses

- **Criterion** (`*_criterion`): Wall-clock timing (cross-platform)
- **Valgrind** (`*_valgrind`): CPU instructions, cache behavior, heap profiling (Linux only)

## Running Benchmarks

### macOS (via Docker)

```bash
# Run all benchmarks in Docker, results copied to target/
./crates/core/bench/macos.sh
```

Docker build:

```bash
docker build -f crates/core/bench/Dockerfile -t rblib-core-bench .
```

### Linux (native)

```bash
# Criterion only
cargo bench -p rblib-core

# All including Valgrind (requires valgrind + gungraun-runner)
cargo bench -p rblib-core --bench apply_valgrind --bench revert_valgrind --bench traversal_valgrind
```

or a specific benchmark:

```bash
cargo bench -p rblib-core --bench apply_criterion
```

## Results

### Criterion

Results in `target/criterion/` with HTML reports.

### Valgrind (via Gungraun)

Results in `target/gungraun/`.

```
apply_valgrind::tx_execution::single_state tx_10:setup_10()
Instructions:  413062  |  506572           (-18.45%) [-1.23x]
               ^ current  ^ checkpoint     ^ single_state is 18% faster
```

- Negative percentage = baseline (revm) uses fewer instructions
- `[-1.23x]` = checkpoint uses 1.23x more instructions

