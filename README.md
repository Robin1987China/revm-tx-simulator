# revm-txsim — 本地交易模拟器

用 [revm](https://github.com/bluealloy/revm) 在**本地**重放一笔真实交易：喂进去一份 tx（calldata + from/to + 区块高度），吐出**状态变化（storage diff）、ETH 余额变化、ERC20 余额变化**。不需要 fork 节点、不需要 Foundry，只依赖一个**可读历史状态的 JSON-RPC**。

> **不保证任何投资用途。** 这是一个本地分析/测量工具，不提供交易信号、不接入交易所、不执行任何链上动作。所有输出仅供研究参考，请勿据此做出投资决策。

---

## 1. 它能做什么

| 能力 | 说明 |
|---|---|
| 交易重放 | 按真实 tx 的 `from/to/value/data/nonce/gas/费用` 在指定区块环境里执行 |
| storage diff | 只列出**实际发生改变**的 storage slot（before → after） |
| ETH 余额变化 | 每个被触碰账户的 before / after / delta + 方向 |
| ERC20 余额变化 | 从执行产生的 `Transfer` 日志解码，按 `(token, account)` 聚合净变化 |
| 真实校验 | 与链上 receipt 的 `status` / `gasUsed` / log 条数自动比对 |
| 反例防护 | nonce 与父块状态不一致时**直接报错**，而不是给出错误答案（见 §6） |

## 2. 环境要求

- Rust（实测 `cargo 1.98.1` / `rustc 1.98.1`）
- crates 走你的 cargo 源（如 rsproxy / 清华镜像），无需额外配置
- **一个支持历史状态（archive）的 JSON-RPC endpoint**

### RPC 需要什么（如实说明）

要模拟历史区块的交易，节点必须能返回**旧区块的账户/存储**。免费公开节点里大多数不支持 archive。实测结果：

| Endpoint | 普通查询 | 历史状态（archive） | 结论 |
|---|---|---|---|
| `https://eth.drpc.org` | ✅ | ✅ | **本工程默认，实测可用** |
| `https://ethereum.publicnode.com` | ✅ | ❌ `Archive requests require a personal token` | 需付费 token |
| `https://1rpc.io/eth` | ✅ | ❌ 免费额度限流 | 需 API key |
| `https://rpc.flashbots.net` | ✅ | 未测（不保证 archive） | — |
| `https://eth.llamarpc.com` | ❌ 525 | — | 不可用 |
| `https://cloudflare-eth.com` | ❌ | — | 不可用 |

**不需要任何 API key**（drpc 免费层实测足够）。如果你要用别的节点，只要它能服务 archive 请求即可；`--rpc <url>` 或环境变量 `REVM_TXSIM_RPC` 覆盖，也支持把 `rpc_url` 写进输入文件。

> 无 archive 时的降级路径（未实现）：先 `debug_traceTransaction` 拿 touched 账户，再逐个 `eth_getProof`。当前版本不做这个。

## 3. 构建 & 测试

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build            # 编译
cargo test             # 20 个测试全过（14 单测 + 6 集成）
```

测试全部**离线**运行（内存数据库 + 真 revm），不依赖网络：

```
running 14 tests (src/lib.rs 单元测试) ....... ok
running  6 tests (tests/sim_tests.rs 集成) ... ok
   test calldata_change_changes_storage_diff ... ok   <-- 判别力验证
```

## 4. 用法

### 4.1 抓一笔真实交易 → 生成输入文件

```bash
./target/debug/revm-txsim fetch <TX_HASH> --rpc https://eth.drpc.org --out input-<name>.json
```

它会把 `eth_getTransactionByHash` + `eth_getTransactionReceipt` 的结果整理成可直接运行的输入文件，并把 receipt 的 `status`/`gasUsed`/log 数写进 `expect` 字段作为基准。

### 4.2 模拟并输出差分

```bash
./target/debug/revm-txsim run --input input-<name>.json --out run-<name>.json
```

选项：

| 选项 | 说明 |
|---|---|
| `--rpc URL` | 覆盖 endpoint（优先级：CLI > 文件 > `$REVM_TXSIM_RPC` > 默认 drpc） |
| `--out FILE` | 同时把 JSON 结果写到文件 |
| `--spec NAME` | 强制硬分叉（`prague`/`osaka`/`cancun`/…）。默认按区块号自动判定 |
| `--no-nonce-check` | 跳过 nonce 校验（**危险**，见 §6） |

一个可直接运行的样例输入在 `examples/erc20-transfer-pyusd.json`：

```bash
./target/debug/revm-txsim run --input examples/erc20-transfer-pyusd.json
```

## 5. 实测结果（3 笔真实主网交易，全部与 receipt 完全一致）

| # | 交易 | 区块 | 类型 | 模拟 gas | receipt gas | logs | 结论 |
|---|---|---|---|---|---|---|---|
| 1 | `0x5baf6895…cfe7` | 26144826 | PYUSD `transfer` | **48576** | 48576 | 1/1 | ✅ 完全一致 |
| 2 | `0x41975150…cc0c` | 26144829 | UniswapV2 路由 swap | **166793** | 166793 | 6/6 | ✅ 完全一致 |
| 3 | `0x6338b5dd…a44f` | 26144829 | UniswapV2 路由 swap | **163280** | 163280 | 6/6 | ✅ 完全一致 |

以 #1（PYUSD 转账）为例，模拟输出与链上日志逐项对得上：

- **ERC20**：`0xe087b3…66a4 → 0x85f053…3925`，`3,630,444`（= `0x37656c`，6 位小数 → 3.630444 PYUSD），与 `Transfer` 日志的 data 完全一致。
- **storage diff**：token 合约两个 slot 变化，sender 余额 slot `0x4c4b40 (5,000,000)` → `0x14e5d4 (1,369,556)`，**差值恰为 3,630,444**；接收方 slot 反向 +3,630,444。
- **ETH**：sender `-115,739,272,294,272 wei`；coinbase `+108,837,974,758,656 wei`。前者 = `gas_used × effective_gas_price`，后者 = `gas_used × max_priority_fee_per_gas`（`0x858c664c`），**分毫不差**，说明费用记账也正确。
- **代币身份已链上核实**：`name()=PayPal USD`、`symbol()=PYUSD`、`decimals()=6`（不靠手写标签）。

复现：#1 的输入即 `examples/erc20-transfer-pyusd.json`；其余两笔用 §4.1 的 `fetch` 从上面列出的 tx hash 重新抓取即可。

## 6. 已知限制（如实列出）

1. **状态快照取父块（block-1）**。最准确的做法是"区块内该 tx 之前的所有 tx 都执行完"的状态，但那要求节点能按 tx index 取状态。当前实现用 `block-1` 的账户/存储。
   - **工具的防护是 nonce 检查**：如果 sender 在同一区块里还有更早的 tx，父块 nonce 会偏低 → revm 报 `NonceTooHigh`，程序**直接失败**而不是给出错误结果。
   - 实测案例：`0x2ff5e609…bfaf`（区块 26144828）因 sender `0x43fc55…f6d2` 在该区块内有 **2 笔**交易（`0xfb517292` 在前）而触发该错误。用 `--no-nonce-check` 强行跑会得到 `gas 54408 / logs 0`，而真实是 `gas 120454 / logs 6` —— **故意留作反例**，证明这个护栏是必要的。
   - 因此当前结论：**同块有前置依赖的交易会被正确拒绝，而不是被静默算错**。
2. **硬分叉自动判定**基于硬编码的激活区块（Shanghai/Cancun/Prague/Osaka）。若未来有新分叉或 L2，需 `--spec` 手工指定。
3. **失败交易（revert）**目前只报告 `success=false` 与 gas，不回溯 revert 原因字符串（未做 return-data 解码）。
4. **ERC20 净变化来自日志**，因此不产生 `Transfer` 事件的非标准代币（或纯 storage 变化的 WETH `deposit/withdraw` 类）不会被列出；`erc20_balance_slots` 提供了按 storage slot 直读 `balanceOf` 的交叉校验通道作为补充。
5. 仅支持 Ethereum 主网语义（`revm` 默认 mainnet builder）；L2 / OP-stack 需要换 builder。
6. `blob` 交易的 blob 数据本身不加载（只按 versioned hash 参与计费），因此依赖 blob 内容的场景不适用。
7. 依赖一个可用的 archive RPC；对公网的稳定性/限流没有保证，失败时直接报错而非静默降级。

## 7. 设计说明（为什么这么做）

- **自带 `DatabaseRef`（`src/rpc.rs`）而不是用现成的 alloy 异步 DB**：revm 的执行 API 是同步的，而"执行过程中惰性拉取状态"必须能在同步栈里发起 HTTP。直接用阻塞式 `ureq` 实现 `DatabaseRef`，然后套 `CacheDB` 做缓存，最简单也最可控（用到的 RPC：`eth_getBalance` / `eth_getTransactionCount` / `eth_getCode` / `eth_getStorageAt` / `eth_getBlockByNumber`）。
- **差分逻辑是纯函数（`src/diff.rs`）**：输入是 revm 的 post-state + 一份 pre-state 快照，不碰 IO，因此可以单测。
- **pre-state 来源**：`transact()` 不会写回 DB，所以执行结束后 `CacheDB` 内部缓存里仍是**加载过的交易前状态**，直接拿来算 before/after，避免二次 RPC。
- **ERC20 用日志而不是猜 storage slot**：不同代币 `balanceOf` 的 slot 位置不同（USDC=9，多数=0），从 `Transfer` 事件解码最通用，且与链上日志天然可对账。
- **依赖精简**：`revm` 关掉默认的 C 语言后端（c-kzg/blst），只留 `std + secp256k1`，编译快、无需 C 工具链。

## 8. 目录结构

```
.
├── Cargo.toml
├── README.md                     ← 本文件
├── LICENSE                       ← MIT
├── src/
│   ├── main.rs                   CLI（fetch / run）
│   ├── lib.rs                    库入口（供集成测试调用）
│   ├── rpc.rs                    阻塞 JSON-RPC 客户端 + revm DatabaseRef
│   ├── model.rs                  输入文件 schema
│   ├── sim.rs                    环境构建 + 单笔交易执行
│   ├── diff.rs                   纯函数差分（balance / nonce / storage）
│   ├── erc20.rs                  Transfer 日志解码 + 净变化聚合 + mapping slot
│   └── hexutil.rs                hex/整数解析
├── tests/sim_tests.rs            集成测试（含判别力验证）
└── examples/                     手写输入样例
```

## 9. 下一步（未做，按价值排序）

1. 用 `debug_traceTransaction` + `eth_getProof` 支持**区块内精确状态**（消除 §6.1 限制）。
2. 失败交易 decoded revert reason（`0x08c379a0` / custom error）。
3. 把单笔扩展为**一笔交易多个候选**：给定同一状态的 N 个 calldata，批量出 diff 排行榜（MEV 搜参雏形）。
4. `gas_used` 与 receipt 不一致时自动 dump 触碰到的账户集合，方便定位同块依赖。

## License

MIT — see [LICENSE](LICENSE).
