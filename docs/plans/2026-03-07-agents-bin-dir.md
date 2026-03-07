# Agents Bin Dir Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 保持默认安装根和缓存仍在 `~/.acm`，但把默认激活后的 CLI 入口改到 `~/.agents/bin`。

**Architecture:** 在客户端上下文里显式区分 `install_dir` 和 `bin_dir`。核心安装引擎不再从 `install_dir/bin` 隐式推导入口目录，而是接收独立的 `bin_dir` 参数；只有默认路径时才把 bin 根定向到 `~/.agents/bin`，显式 `--install-dir` 仍沿用 `<install-dir>/bin`。

**Tech Stack:** Rust workspace、acm-core 安装引擎、acm-client CLI、GitHub Actions E2E。

---

### Task 1: 固化默认 bin 路径行为

**Files:**
- Modify: `crates/acm-client/src/lib.rs`
- Test: `crates/acm-client/src/lib.rs`

**Step 1: Write the failing test**
- 为默认路径解析新增测试：`default_install_dir()` 仍返回 `~/.acm`，新的默认 `bin_dir` 返回 `~/.agents/bin`。
- 为显式 `--install-dir` 保留原行为新增测试：自定义安装目录时 bin 目录仍为 `<install-dir>/bin`。

**Step 2: Run test to verify it fails**
Run: `cargo test -p acm-client default_bin_dir`
Expected: FAIL，因为还没有独立 bin 目录逻辑。

**Step 3: Write minimal implementation**
- 在 `InstallContext` 中加入 `bin_dir`。
- 新增默认 bin 根解析函数，只在默认安装根时返回 `~/.agents/bin`。
- `run_with_args()` 构造上下文时同时解析 `install_dir` 与 `bin_dir`。

**Step 4: Run test to verify it passes**
Run: `cargo test -p acm-client default_bin_dir`
Expected: PASS。

### Task 2: 让安装引擎显式使用 bin_dir

**Files:**
- Modify: `crates/acm-core/src/install_engine.rs`
- Modify: `crates/acm-client/src/commands.rs`
- Test: `crates/acm-core/src/install_engine.rs`

**Step 1: Write the failing test**
- 新增安装/卸载测试：`install_dir` 指向 `~/.acm` 风格目录，`bin_dir` 指向独立 `~/.agents/bin`，安装后入口文件应出现在独立 bin 目录，卸载后也应从该目录删除。

**Step 2: Run test to verify it fails**
Run: `cargo test -p acm-core install_engine::tests::test_install_and_uninstall_with_separate_bin_dir`
Expected: FAIL，因为当前引擎总是写到 `install_dir/bin`。

**Step 3: Write minimal implementation**
- 给 `InstallRequest`、`ImportRequest`、`UninstallRequest`、`UpdateRequest` 增加独立 `bin_dir`。
- `activate_executable()`、`bin_path_for_provider()`、卸载逻辑全部改用显式 `bin_dir`。
- 调整 `acm-client` 的调用点传递 `ctx.bin_dir`。

**Step 4: Run test to verify it passes**
Run: `cargo test -p acm-core install_engine::tests::test_install_and_uninstall_with_separate_bin_dir`
Expected: PASS。

### Task 3: 更新用户可见输出与诊断

**Files:**
- Modify: `crates/acm-client/src/commands.rs`
- Modify: `.github/scripts/e2e-install-unix.sh`
- Modify: `.github/scripts/e2e-install-windows.ps1`

**Step 1: Write the failing test**
- 给 doctor / 路径提示补测试，确保默认情况下检查的是 `~/.agents/bin`。
- 如无合适单测入口，则先用现有针对性脚本复现作为红灯。

**Step 2: Run test to verify it fails**
Run: `cargo test -p acm-client doctor` 或针对性复现命令
Expected: FAIL，bin 检查仍指向 `~/.acm/bin`。

**Step 3: Write minimal implementation**
- `commands.rs` 中 PATH 提示和 doctor 的 `bin_dir_in_path` 改用 `ctx.bin_dir`。
- 如 E2E 脚本有硬编码路径，改为与默认/显式行为一致。

**Step 4: Run test to verify it passes**
Run: `cargo test -p acm-client doctor` 或针对性复现命令
Expected: PASS。

### Task 4: 全量验证

**Files:**
- Modify: none
- Test: workspace + 关键脚本复现

**Step 1: Run focused tests**
Run: `cargo test -p acm-client && cargo test -p acm-core install_engine`
Expected: PASS。

**Step 2: Run quality checks**
Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --all-targets`
Expected: PASS。

**Step 3: Run targeted behavior repro**
Run: 使用隔离 `HOME` 运行默认安装，确认产物仍在 `~/.acm/providers/...`，但入口在 `~/.agents/bin`。
Expected: PASS。

**Step 4: Commit**
```bash
git add crates/acm-client/src/lib.rs crates/acm-client/src/commands.rs crates/acm-core/src/install_engine.rs .github/scripts/e2e-install-unix.sh .github/scripts/e2e-install-windows.ps1 docs/plans/2026-03-07-agents-bin-dir.md
git commit -m "feat: move default bin dir to agents"
```
