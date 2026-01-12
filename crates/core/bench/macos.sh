#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
CORE_DIR="$WORKSPACE_ROOT/crates/core"

cd "$WORKSPACE_ROOT"

echo "=== Building Docker image ==="
docker build -f crates/core/bench/Dockerfile -t rblib-core-bench .

echo ""
echo "=== Running Criterion benchmarks ==="
docker run --rm \
    -v "$CORE_DIR/target/criterion:/workspace/crates/core/target/criterion" \
    rblib-core-bench \
    cargo bench -p rblib-core \
        --bench apply_criterion \
        --bench revert_criterion \
        --bench traversal_criterion \
        --bench forking

echo ""
echo "=== Running Valgrind benchmarks ==="
docker run --rm \
    -v "$CORE_DIR/target/gungraun:/workspace/target/gungraun" \
    rblib-core-bench \
    cargo bench -p rblib-core \
        --bench apply_valgrind \
        --bench revert_valgrind \
        --bench traversal_valgrind

echo ""
echo "=== Done ==="
echo "Criterion results: crates/core/target/criterion/"
echo "Valgrind results:  crates/core/target/gungraun/"
