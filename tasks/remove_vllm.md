## 背景介绍 和 任务描述

当前代码是我基于vllm-router的commit d60711d，支持了lmdeploy推理引擎，并修复了一些小的bug。现在我想把vllm独有的功能完全移除，完全变成lmdeploy-router，需要移除的功能包括但不限于

1. vllm-pd-disaggregation 功能
2. vllm-discovery-address 功能
3. 文档里面出现vllm相关的关键字
4. 对intra-node-data-parallel-size选项的支持，会在header里面设置一些dp rank相关的字段，lmdeploy不支持，也去掉

不需要移除的功能

1. 很多的负载均衡策略，是推理引擎无关的，不需要移除
2. 通用功能，例如k8s服务发现，超时设置，Circuit Breaker等
3. 目录下有很多未被git纳入管理的临时文件，请忽略它们，不要修改它们


## 设计和实现要求

1. 代码风格：针对代码风格的问题，后续修改到的代码都要按照 yaoqian-working-style skill 进行


## 测试验证

跑单测使用以下命令，看看是否测试全部通过

```
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo test --lib --locked --offline
```

同时跑py_test/integration/lmdeploy/里面的测试，如果这个python测试依赖部署的服务，可以先忽略


## 子任务规划

以下任务按依赖关系和可独立 review 的粒度排列。每个任务完成后单独提交一次；提交前运行该任务声明的聚焦测试，最后统一运行完整验证。

### 任务 1：梳理移除范围（已完成）

**目标**
- 盘点 Rust、Python、测试、文档、打包和 CI 中 vLLM 专属能力与命名。
- 区分必须移除、可保留的引擎无关能力，以及需要用户后续决策的边界项。

**范围**
- 识别 vLLM PD 路由、ZMQ/MoRI-IO 服务发现、DP-aware 路由、协议扩展、日志/指标命名、包名、测试和文档。
- 不修改业务代码。

**验收**
- 产出本任务清单和各阶段验证命令。

### 任务 2：移除 vLLM PD 的外部入口和配置路径（已完成，待 review）

**目标**
- 删除 `--vllm-pd-disaggregation`、`--vllm-discovery-address`、`vllm_pd_disaggregation` 和 `vllm_discovery_address`。
- PD 配置只保留 LMDeploy PD 分支，CLI/Python/PyO3/Rust 配置保持一致。

**范围**
- Rust 配置类型、校验、CLI、PyO3 构造器、RouterFactory、K8s 动态注册和 IGW 注册。
- Python `RouterArgs`、`Router.from_args`、MiniLB 和相关单测。
- 同步保留测试到 `RoutingMode::LMDeployPrefillDecode`。

**明确不做**
- 不删除 `vllm_pd_router.rs` 和 `vllm_service_discovery.rs` 文件；这些实现文件由任务 3 和任务 4 删除。
- 不处理 `intra-node-data-parallel-size`；由任务 5 删除。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo test --lib --locked --offline
cargo test --test test_dp_routing --locked --offline
pytest py_test/unit/test_arg_parser.py \
  py_test/unit/test_router_config.py \
  py_test/unit/test_validation.py \
  py_test/unit/test_startup_sequence.py \
  py_test/test_launch_router.py
```

### 任务 3：删除 vLLM PD Router 和 vLLM logprobs 合并实现

**目标**
- 删除 `src/routers/http/vllm_pd_router.rs`。
- 删除按 vLLM OpenAI 响应结构合并 prompt logprobs 的 `src/routers/http/logprobs_merge.rs`。
- 移除 `src/routers/http/mod.rs` 中对应模块导出和不再使用的测试。

**范围**
- 保留 `pd_router.rs`、`pd_types.rs` 和 `lmdeploy_pd_router.rs`。
- 保留普通路由的 logprobs 透传能力。
- 清理仅测试 vLLM PD 请求构造、vLLM logprobs 合并和 vLLM KV transfer 参数的测试。

**logprobs 语义核查结论**
- LMDeploy 已支持 `return_logprob`、`top_logprobs_num` 和 `logprob_start_len`；
  `/generate` 会返回 `meta_info.input_token_logprobs` 与 `meta_info.output_token_logprobs`。
- 当前 LMDeploy `serve/proxy/proxy.py` 的 DistServe 路径直接返回 decode 响应，
  未合并 prefill 响应中的 input logprobs；Rust 的 LMDeploy PD router 也尚未实现该合并。
- 因此“响应体内合并 input/output logprobs”不是 vLLM 独有概念，但仓库现有
  `logprobs_merge.rs` 的字段结构是 vLLM/OpenAI 专属实现，不能直接复用。
- 任务 3 只删除该 vLLM 专属实现；是否在 `LMDeployPDRouter` 中按
  `meta_info.input_token_logprobs` 补齐 LMDeploy PD 行为，需要先在
  `py_test/integration/lmdeploy` 增加带真实后端的回归用例。默认先不实现，
  避免 LMDeploy 未迁移对应 logits 时伪造或误报 logprob 结果。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo test --lib --locked --offline
rg -n 'VllmPDRouter|vllm_pd_router|logprobs_merge' src tests py_src py_test \
  --glob '!py_test/e2e/pd_disagg_vllm/**'
```

**完成标准**
- 最后一个搜索无结果；LMDeploy PD 与普通路由测试通过。

### 任务 4：删除 vLLM ZMQ 服务发现和二进制依赖

**目标**
- 删除 `src/routers/http/vllm_service_discovery.rs`。
- 删除 `Cargo.toml` 中的 `zmq` 和 `rmp-serde` 依赖。
- 更新 `Cargo.lock`，确认没有任何代码引用 ZMQ registry、MessagePack 注册协议或 `vllm-discovery-address`。

**范围**
- 只删除 vLLM ZMQ worker 发现机制。
- 保留通用 Kubernetes service discovery、K8s pod watch、annotations 解析和动态注册。
- 保留 LMDeploy 自己的 `engine_endpoint_info`/HTTP 注册行为，不做 ZMQ 替代。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo test --lib --locked --offline
rg -n 'zmq|rmp_serde|rmp-serde|vllm_service_discovery' src tests py_src py_test Cargo.toml Cargo.lock
```

**完成标准**
- `Cargo.toml`/`Cargo.lock` 不再包含 `zmq`、`rmp`、`rmp-serde` 及其 ZMQ 传递依赖；
  代码和测试中唯一允许保留的是 LMDeploy API 响应字段中的 `zmq_address` 字符串。

### 任务 5：移除 intra-node DP-aware 路由

**目标**
- 删除 `--intra-node-data-parallel-size` / `router_intra_node_data_parallel_size` 配置项。
- 删除 `DPAwareWorker`、`dp_utils`、URL `@rank` 展开、`X-data-parallel-rank` 请求头和相关校验/测试。

**范围**
- Rust：`RouterConfig`、CLI、PyO3、Regular Router、`PdRouterBase`、`LMDeployPDRouter`、`core::worker`。
- Python：`RouterArgs`、`Router.from_args`、帮助文本和 API key 说明。
- 测试：`tests/test_dp_routing.rs`、`test_startup_sequence.py`、`test_router_config.py`、`test_arg_parser.py`、`test_launch_router.py`。
- 保留普通 API key 校验能力，不再把它与 DP-aware 模式绑定。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo test --lib --locked --offline
pytest py_test/unit/test_arg_parser.py \
  py_test/unit/test_router_config.py \
  py_test/unit/test_startup_sequence.py \
  py_test/test_launch_router.py
rg -n 'intra.node.data.parallel|DPAwareWorker|dp_utils|X-data-parallel-rank|dp_rank|extract_dp_info|create_dp_aware' src py_src tests py_test
```

**完成标准**
- `tests/test_dp_routing.rs` 已删除，上述搜索无路由器源码/测试引用（历史文档记录在任务 12 清理）。
- `dp_size` 仅允许作为 LMDeploy 引擎协议元数据或测试装置自身的引擎参数保留，不再出现在 Rust 路由 DP-aware 实现中。

### 任务 6：精简 LMDeploy PD 基类和 bootstrap-port 语义

**目标**
- 在任务 2/3/5 的边界收敛后，复查 `PdRouterBase` 与 `WorkerType::Prefill`。
- 删除只为 vLLM bootstrap server 服务的 URL 二元组、bootstrap-port 参数、`bootstrap_port_annotation` 默认值和相关解析。

**范围**
- `PdRouterBase` 构造、worker 注册、K8s annotation 读取与 `LMDeployPDRouter` 的调用。
- Python `--prefill URL [PORT]` 改为只接受 URL；确认 CLI 错误信息清晰。
- 保留 LMDeploy migration request 所需的 engine ID / endpoint 信息。

**前置条件**
- 任务 3、4、5 已合并；本任务只处理跨文件残余，避免提前做大范围重构。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo test --lib --locked --offline
cargo test --test test_pd_routing --locked --offline
pytest py_test/unit/test_arg_parser.py py_test/test_launch_router.py
rg -n 'bootstrap.port|bootstrap_port' src py_src tests py_test
```

**完成标准**
- 搜索结果仅剩与 LMDeploy 实际协议相关的字段；不存在 vLLM annotation 或 URL 端口约定。

### 任务 7：清理通用 K8s service discovery 中的 vLLM 语义

**目标**
- 将 K8s service discovery 保持为引擎无关能力，同时删除 vLLM 专属默认 annotation、示例标签和文案。
- 根据任务 6 的结果决定 `bootstrap_port_annotation` 字段是否完全移除。

**范围**
- `ServiceDiscoveryConfig` 默认值、CLI/Python 参数、测试中的 `vllm.ai/bootstrap-port` 和 `app=vllm`。
- 保留 selector、namespace、port、prefill/decode selector 和 pod watch。
- 如 LMDeploy PD 仍需要 annotation，改为中性配置名并用文档说明 LMDeploy 部署约定；如不需要则删除。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo test config:: --lib --locked --offline
cargo test --lib --locked --offline
pytest py_test/unit/test_arg_parser.py py_test/unit/test_router_config.py
rg -n 'vllm\.ai/bootstrap-port|app=vllm|app=vllm-worker|vllm.*selector|selector.*vllm' src py_src tests py_test docs README.md
```

### 任务 8：移除 vLLM backend 枚举和运行时入口

**目标**
- 删除 `Backend::Vllm` 及 `--backend vllm` / `--runtime vllm` 默认值。
- 将普通路由的主路径明确为 LMDeploy OpenAI-compatible backend。
- 复查 `trtllm`、`anthropic`、`openai` 是否为未实现的占位行为；仅保留仍可运行且与 LMDeploy router 定位一致的模式。

**范围**
- `src/main.rs` 的 `Backend` 枚举、默认值、启动日志和未实现 warning。
- 删除或改写 `RoutingMode::OpenAI`/`OpenAIRoute` 仅在实际保留 external OpenAI backend 需求时执行。
- 同步 `src/main.rs` 中的 VLLM Router 标题、进程标题和输出日志。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo run --bin lmdeploy-router --locked --offline -- --help
cargo test --lib --locked --offline
```

**待决策**
- 是否保留 external OpenAI backend。若用户无需，则删除 `RoutingMode::OpenAI`、`OpenAIRouter` 和相关测试。

### 任务 9：清理 vLLM 专属协议命名与扩展语义

**目标**
- 将仍然有效的 OpenAI-compatible 参数从 “VLLM extension” 改为引擎无关命名，或确认 LMDeploy 不支持后删除。
- 删除 vLLM 原生 generate/rerank 协议中与 LMDeploy 不兼容的结构和请求改写路径。

**范围**
- `src/protocols/spec.rs`、`src/protocols/validation.rs` 中的 `VLLMExtensionsProvider`、`constants::vllm`、`lora_path`、`chat_template_kwargs`、vLLM native generate/rerank 类型。
- `src/otel_http.rs` 中 `vllm.request_phase` 语义属性名。
- 保留普通 OpenAI API 校验、input_ids 扩展和 LMDeploy tokenizer/转发所需字段。
- 保留 `Responses API`、rerank 等通用功能仅在明确属于 LMDeploy 兼容矩阵时。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo test --lib --locked --offline
cargo test --test request_formats_test --locked --offline
cargo test --test responses_api_test --locked --offline
rg -n 'VLLM|Vllm|vllm' src/protocols src/otel_http.rs src/server.rs
```

**注意**
- 不得仅因名字包含 vLLM 而删除 LMDeploy 也接受的采样参数；必须以 LMDeploy API 兼容性为准。

### 任务 10：删除和替换 vLLM 专属测试资产

**目标**
- 删除 `py_test/e2e/pd_disagg_vllm/` 和 `py_test/e2e/test_pd_router.py`。
- 清理测试 fixture、mock worker、注释、标记和输出中的 vLLM 专属启动方式。
- 保留 LMDeploy 集成测试、通用路由测试和引擎无关 mock。

**范围**
- `py_test/e2e/conftest.py`、`py_test/fixtures/router_manager.py`、`tests/common/mock_worker.rs`、相关 Rust 测试注释。
- 将仍有效的 PD 测试统一指向 `LMDeployPDRouter`。
- 删除只覆盖 vLLM 请求构造、KV connector、ZMQ discovery、DP header 的测试。

**聚焦验证**
```bash
pytest py_test/integration/lmdeploy
pytest py_test/unit/test_arg_parser.py \
  py_test/unit/test_router_config.py \
  py_test/unit/test_validation.py \
  py_test/unit/test_startup_sequence.py \
  py_test/test_launch_router.py
cargo test --lib --locked --offline
```

### 任务 11：完成 Python/Rust 包名和入口重命名

**目标**
- 将用户可见包名、模块名、安装名和 CLI 从 `vllm-router` / `vllm_router` / `vllm_router_rs` 改为 `lmdeploy-router` 对应命名。
- 保持二进制名 `lmdeploy-router` 不变。

**范围**
- `pyproject.toml` 的 project name、script、description。
- `Cargo.toml` crate name；`src/main.rs` import。
- `py_src/vllm_router/` 目录重命名为 `py_src/lmdeploy_router/`。
- Python import、`setproctitle`、日志文件名、setup.py Rust extension target。
- Buildkite 构建产物与 nightly 命名。

**聚焦验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo check --lib --locked --offline
cargo run --bin lmdeploy-router --locked --offline -- --help
python -m pytest py_test/unit/test_arg_parser.py py_test/test_launch_router.py
git ls-files | xargs rg -n 'vllm-router|vllm_router|vllm_router_rs' \
  --glob '!target/**' --glob '!Cargo.lock'
```

**注意**
- 这是破坏性 API 兼容变更；执行前确认不再需要旧 Python 包名兼容层。
- `__pycache__` 和未跟踪临时文件不纳入修改。

### 任务 12：清理文档、README 和最终命名残留

**目标**
- 删除 README、docs、CI 说明、代码注释中的 vLLM 关键词。
- 明确产品定位为 LMDeploy Router，同时说明保留的 OpenAI-compatible 协议和通用能力。

**范围**
- `README.md`、`docs/lmdeploy_support_guide.md`、`docs/load_balancing/README.md`、`src/tokenizer/README.md`。
- `.buildkite/` 下的文档与 pipeline 说明。
- 更新安装命令、示例、架构图、指标名、日志名和日志文件名。
- 删除指向 vLLM 上游 KV transfer 的专属链接。

**聚焦验证**
```bash
git ls-files | xargs rg -n -i 'vllm' \
  --glob '!target/**' --glob '!Cargo.lock' --glob '!tasks/remove_vllm.md'
```

**完成标准**
- 除该任务文件外，Git tracked 文件无 `vllm` 关键词（大小写不敏感）。

### 任务 13：全量回归、lint 和依赖审计

**目标**
- 在所有移除任务完成后执行完整验证，并可选修复已有 clippy 问题。

**范围与验证**
```bash
export CARGO_HOME=/data/cargo-cache
cargo fmt --check
cargo check --lib --locked --offline
cargo test --lib --locked --offline
cargo test --locked --offline
cargo clippy --lib --all-features --locked --offline -- -D warnings
ruff check py_src/ py_test/
pytest py_test/integration/lmdeploy
pytest py_test/unit py_test/test_launch_router.py
```

**当前已知问题**
- `create_lmdeploy_pd_router` 参数数超过 clippy 默认阈值。
- `LMDeployPDRouter.p2p_pool` 类型别名过深。
- 这两处可在本阶段以小范围结构化参数或类型别名修复，避免在功能删除任务中混入重构。

**最终审计**
```bash
rg -n -e 'vllm_pd_disaggregation' -e 'vllm_discovery_address' \
  -e 'VllmPDRouter' -e 'vllm_service_discovery' \
  -e 'intra.node.data.parallel' -e 'X-data-parallel-rank' \
  $(git ls-files src py_src tests py_test docs README.md)
```

**完成标准**
- 所有测试和 lint 通过；如外部环境不可用，跳过说明必须记录在后验结果中，但不能跳过可本地执行的测试。
