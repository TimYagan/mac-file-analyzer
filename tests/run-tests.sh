#!/bin/bash

# run-tests.sh - Script to run the test suite for mac-file-analyzer
# Usage: ./run-tests.sh [options]
# Options are passed directly to cargo test

set -e  # Exit on any error

echo "Running tests for mac-file-analyzer..."
echo "======================================"

# Run the tests with any provided arguments
cargo test "$@"

echo "======================================"
echo "All tests completed successfully!"