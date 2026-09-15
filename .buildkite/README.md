# Buildkite CI/CD Configuration

This directory contains the Buildkite pipeline configurations for the LMDeploy Router project.

## Pipeline Files

### `pipeline.yml`
Main CI/CD pipeline that runs on all commits and pull requests. Includes:

- **Fast Checks**: Code formatting and linting (Rust, Python)
- **Build**: Release builds for Rust binary and Python wheels
- **Tests**: Comprehensive test suite (unit, integration, Python)
- **P/D Disaggregation Test**: GPU-based integration test for prefill/decode disaggregation
- **Benchmarks**: Optional performance benchmarks
- **Docker Build**: Container image creation

### `release-pipeline.yml`
Release pipeline triggered on version tags (e.g., `v1.2.3`). Handles:

- Building release artifacts
- Publishing to PyPI
- Building and pushing Docker images
- Creating GitHub releases

## P/D Disaggregation Test

The P/D (Prefill/Decode) disaggregation test validates the router's ability to coordinate separate LMDeploy Prefill and Decode instances.

### How It Works

The test expects externally managed LMDeploy services:

1. **Prefill instance(s)**: LMDeploy servers started with `--role Prefill`
2. **Decode instance(s)**: LMDeploy servers started with `--role Decode`
3. **Router**: Started by the test with `--lmdeploy-pd-disaggregation`

The migration backend and RDMA environment must be provisioned outside the router test.

### Test Script

Location: `py_test/e2e/pd_disagg_lmdeploy/run_accuracy_test.sh`

The test script:

- Starts the router with LMDeploy P/D disaggregation
- Registers configured Prefill and Decode URLs
- Runs health, completion, and streaming validation
- Cleans up the router on exit

### CI Configuration

The test runs in `.buildkite/pipeline.yml` when `PREFILL_URLS` and `DECODE_URLS` are supplied.

**Key features:**

- External Prefill/Decode services with a matching migration backend
- Automatic retry options are configured in the pipeline
- Router logs are collected as `/tmp/lmdeploy-pd-router.log`

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `PREFILL_URLS` | Yes | Comma-separated Prefill API URLs |
| `DECODE_URLS` | Yes | Comma-separated Decode API URLs |
| `MODEL_PATH` | No | Model path used by the external services |
| `MODEL_NAME` | Yes | Model name exposed by `/v1/models` |
| `LMDEPLOY_ROUTER_BIN` | No | Router binary path |
| `MIGRATION_PROTOCOL` | No | `rdma` or `nvlink`; default is `rdma` |
| `RDMA_LINK_TYPE` | No | `roce` or `ib`; default is `roce` |

### Artifacts

On test completion (success or failure), artifacts are collected:

- `/tmp/router.log`: Router process logs

### Running Locally

To run the test locally:

```bash
cd py_test/e2e/pd_disagg_lmdeploy

# Run with defaults
bash ./run_accuracy_test.sh

# Run with custom configuration
export GPU_MEMORY_UTILIZATION=0.8
export PREFILLER_TP_SIZE=2
export DECODER_TP_SIZE=2
bash ./run_accuracy_test.sh
```

**Requirements:**
- 4+ GPUs
- Docker with GPU support
- LMDeploy router binary in PATH

### Debugging Failed Tests

1. **Check router logs**: Download `/tmp/router.log` from Buildkite artifacts
2. **Check container logs**: The test script outputs logs on failure
3. **Verify GPU availability**: Run `nvidia-smi` to check GPU status
4. **Retry**: Use manual retry if failure appears infrastructure-related
5. **Run locally**: Reproduce the issue with the same configuration

## Additional Pipeline Steps

### Fast Checks
Runs in parallel for quick feedback:
- Rust format check (`cargo fmt`)
- Clippy linting (`cargo clippy`)
- Python format check (black, ruff)

### Build
Creates release artifacts:
- Rust binary (`target/release/lmdeploy-router`)
- Python wheels and source distribution

### Tests
Comprehensive test suite:
- Rust unit tests
- Rust integration tests
- Python tests with coverage

### Benchmarks
Optional manual trigger for performance benchmarks.

### Docker Build
Builds Docker image for the router.

## Agent Queues

- `cpu_queue_premerge`: CPU-only tasks (builds, lints, unit tests)
- `gpu_4_queue`: GPU tests requiring 4+ GPUs
- `default`: General purpose queue

## Adding New Tests

To add a new test step:

1. Choose appropriate location in pipeline
2. Define test command and dependencies
3. Specify agent queue
4. Add artifact collection
5. Consider retry logic for flaky tests
6. Update this documentation

Example:
```yaml
- label: ":test_tube: New Test"
  command: |
    # Test commands here
  agents:
    queue: "cpu_queue_premerge"
  depends_on: "build"
  artifact_paths:
    - "test-results/**/*"
```

## Environment Variables

Buildkite provides these built-in variables:

- `BUILDKITE_COMMIT`: Git commit SHA
- `BUILDKITE_TAG`: Git tag (for releases)
- `BUILDKITE_BRANCH`: Git branch name
- `BUILDKITE_BUILD_NUMBER`: Build number

## Additional Resources

- [Buildkite Documentation](https://buildkite.com/docs)
- [LMDeploy Documentation](https://lmdeploy.readthedocs.io/)
- [Project README](../README.md)
- [Example P/D Setup Scripts](../scripts/llama3.1/)
