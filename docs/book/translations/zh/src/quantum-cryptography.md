# 量子密码学 — 后量子威胁分析

本章分析量子计算机将如何影响 GroveDB 及其上构建的隐私交易协议（Orchard、
Dash Platform）所使用的密码学原语。涵盖哪些组件易受攻击、哪些是安全的、
"先收集后解密"对已存储数据意味着什么，以及存在哪些缓解策略——包括混合 KEM
设计。

## 两个重要的量子算法

实际上只有两个量子算法与密码学相关：

**Shor 算法** 在多项式时间内解决离散对数问题（和整数分解问题）。对于像
Pallas 这样的 255 位椭圆曲线，大约需要 510 个逻辑量子比特——但加上纠错
开销，实际需求约为 400 万个物理量子比特。Shor 算法**完全破解**所有椭圆曲线
密码学，与密钥大小无关。

**Grover 算法** 为暴力搜索提供二次加速。256 位对称密钥实际上变为 128 位。
然而，对 128 位密钥空间运行 Grover 算法的电路深度仍为 2^64 次量子操作——
许多密码学家认为由于退相干限制，这在实际硬件上永远不会实用。Grover 降低了
安全边际，但不会破解参数化良好的对称密码学。

| 算法 | 目标 | 加速 | 实际影响 |
|------|------|------|---------|
| **Shor** | ECC 离散对数、RSA 分解 | 指数级（多项式时间） | **完全破解** ECC |
| **Grover** | 对称密钥搜索、哈希原像 | 二次（密钥位数减半） | 256 位 → 128 位（仍然安全） |

## GroveDB 的密码学原语

GroveDB 和基于 Orchard 的隐私协议混合使用了椭圆曲线原语和对称/哈希原语。
下表按量子脆弱性对每个原语进行分类：

### 量子脆弱（Shor 算法 — 后量子安全性为 0 位）

| 原语 | 使用位置 | 破解内容 |
|------|---------|---------|
| **Pallas ECDLP** | 票据承诺（cmx）、临时密钥（epk/esk）、查看密钥（ivk）、支付密钥（pk_d）、作废符派生 | 从公钥恢复任意私钥 |
| **ECDH 密钥协商**（Pallas） | 为票据密文派生对称加密密钥 | 恢复共享密钥 → 解密所有票据 |
| **Sinsemilla 哈希** | CommitmentTree 默克尔路径、电路内哈希 | 抗碰撞性依赖 ECDLP；Pallas 被破解时退化 |
| **Halo 2 IPA** | ZK 证明系统（基于 Pasta 曲线的多项式承诺） | 伪造虚假陈述的证明（伪造、未授权支出） |
| **Pedersen 承诺** | 隐藏交易金额的价值承诺（cv_net） | 恢复隐藏金额；伪造余额证明 |

### 量子安全（Grover 算法 — 128+ 位后量子安全性）

| 原语 | 使用位置 | 后量子安全性 |
|------|---------|------------|
| **Blake3** | Merk 树节点哈希、MMR 节点、BulkAppendTree 状态根、子树路径前缀 | 128 位原像安全、128 位第二原像安全 |
| **BLAKE2b-256** | 对称密钥派生的 KDF、出站密码密钥、PRF^expand | 128 位原像安全 |
| **ChaCha20-Poly1305** | 加密 enc_ciphertext 和 out_ciphertext（256 位密钥） | 128 位密钥搜索（安全，但通过 ECDH 的密钥派生路径不安全） |
| **PRF^expand**（BLAKE2b-512） | 从 rseed 派生 esk、rcm、psi | 128 位 PRF 安全性 |

### GroveDB 基础设施：完全量子安全

GroveDB 自身的所有数据结构完全依赖 Blake3 哈希：

- **Merk AVL 树** — 节点哈希、combined_value_hash、子哈希传播
- **MMR 树** — 内部节点哈希、峰值计算、根派生
- **BulkAppendTree** — 缓冲区哈希链、密集默克尔根、纪元 MMR
- **CommitmentTree 状态根** — `blake3("ct_state" || sinsemilla_root || bulk_state_root)`
- **子树路径前缀** — 路径段的 Blake3 哈希
- **V1 证明** — 通过 Merk 层级结构的认证链

**无需更改。** GroveDB 的 Merk 树证明、MMR 一致性检查、BulkAppendTree 纪元
根以及所有 V1 证明认证链在面对量子计算机时仍然安全。基于哈希的基础设施是
系统在后量子时代最坚固的部分。

## 追溯性威胁与实时威胁

这一区分对于确定修复优先级至关重要。

**追溯性威胁**危害已经存储的数据。对手今天记录数据，在量子计算机可用时解密。
这些威胁**事后无法缓解**——一旦数据上链，就无法重新加密或撤回。

**实时威胁**仅影响未来创建的交易。拥有量子计算机的对手可以伪造签名或证明，
但仅限于新交易。旧交易已经被网络验证和确认。

| 威胁 | 类型 | 暴露内容 | 紧迫性 |
|------|------|---------|--------|
| **票据解密**（enc_ciphertext） | **追溯性** | 票据内容：收款人、金额、备注、rseed | **高** — 永久存储 |
| **价值承诺打开**（cv_net） | **追溯性** | 交易金额（但非发送方/接收方） | **中** — 仅金额 |
| **发送方恢复数据**（out_ciphertext） | **追溯性** | 已发送票据的发送方恢复密钥 | **高** — 永久存储 |
| 支出授权伪造 | 实时 | 可伪造新的支出签名 | 低 — 在量子计算机到来前升级 |
| Halo 2 证明伪造 | 实时 | 可伪造新证明（伪造品） | 低 — 在量子计算机到来前升级 |
| Sinsemilla 碰撞 | 实时 | 可伪造新的默克尔路径 | 低 — 被证明伪造所包含 |
| 绑定签名伪造 | 实时 | 可伪造新的余额证明 | 低 — 在量子计算机到来前升级 |

### 具体会暴露什么？

**如果票据加密被破解**（主要 HNDL 威胁）：

量子对手通过 Shor 算法从存储的 `epk` 恢复 `esk`，计算 ECDH 共享密钥，
派生对称密钥，并解密 `enc_ciphertext`。这将揭示完整的票据明文：

| 字段 | 大小 | 揭示内容 |
|------|------|---------|
| version | 1 字节 | 协议版本（不敏感） |
| diversifier | 11 字节 | 收款人地址组件 |
| value | 8 字节 | 确切交易金额 |
| rseed | 32 字节 | 允许作废符关联（去匿名化交易图） |
| memo | 36 字节（DashMemo） | 应用数据，可能具有识别性 |

有了 `rseed` 和 `rho`（与密文一起存储），对手可以计算 `esk = PRF(rseed, rho)`
并验证临时密钥绑定。结合 diversifier，这将输入与输出在整个交易历史中关联起来
——**完全去匿名化隐私池**。

**如果仅价值承诺被破解**（次要 HNDL 威胁）：

对手通过解决 ECDLP 从 `cv_net = [v]*V + [rcv]*R` 恢复 `v`。这揭示了
**交易金额，但不揭示发送方或接收方身份**。对手看到"某人向某人发送了 5.0 Dash"
但如果不同时破解票据加密，就无法将金额与任何地址或身份关联。

金额本身在没有关联的情况下用途有限。但结合外部数据（时间、已知发票、与公开
请求匹配的金额），关联攻击就成为可能。

## "先收集后解密"攻击

这是最紧迫和最实际的量子威胁。

**攻击模型：**国家级对手（或任何拥有足够存储的一方）记录今天区块链上所有隐私
交易数据。这些数据在区块链上公开可用且不可更改。对手等待密码学相关量子计算机
（CRQC）出现，然后：

```text
Step 1: Read stored record from CommitmentTree BulkAppendTree:
        cmx (32) || rho (32) || epk (32) || enc_ciphertext (104) || out_ciphertext (80)

Step 2: Solve ECDLP on Pallas via Shor's algorithm:
        epk = [esk] * g_d  →  recover esk

Step 3: Compute shared secret:
        shared_secret = [esk] * pk_d

Step 4: Derive symmetric key (BLAKE2b is quantum-safe, but input is compromised):
        K_enc = BLAKE2b-256("Zcash_OrchardKDF", shared_secret || epk)

Step 5: Decrypt enc_ciphertext using ChaCha20-Poly1305:
        → version || diversifier || value || rseed || memo

Step 6: With rseed + rho, link nullifiers to note commitments:
        esk = PRF(rseed, rho)
        → full transaction graph reconstruction
```

**关键洞察：**对称加密（ChaCha20-Poly1305）是完全量子安全的。脆弱性完全在于
**密钥派生路径**——对称密钥从 ECDH 共享密钥派生，而 ECDH 被 Shor 算法破解。
攻击者不是破解加密；而是恢复密钥。

**追溯性：**这种攻击**完全具有追溯性**。一旦 CRQC 存在，链上存储的每一个
加密票据都可以被解密。数据事后无法重新加密或保护。这就是为什么必须在数据
存储之前而非之后解决此问题。

## 缓解措施：混合 KEM（ML-KEM + ECDH）

对 HNDL 的防御是从**两个独立的密钥协商机制**派生对称加密密钥，使得仅破解
其中一个不足以获取密钥。这称为混合 KEM。

### ML-KEM-768（CRYSTALS-Kyber）

ML-KEM 是 NIST 标准化（FIPS 203，2024 年 8 月）的后量子密钥封装机制，
基于模格学习含错（MLWE）问题。

| 参数 | ML-KEM-512 | ML-KEM-768 | ML-KEM-1024 |
|------|-----------|-----------|------------|
| 公钥（ek） | 800 字节 | **1,184 字节** | 1,568 字节 |
| 密文（ct） | 768 字节 | **1,088 字节** | 1,568 字节 |
| 共享密钥 | 32 字节 | 32 字节 | 32 字节 |
| NIST 类别 | 1（128 位） | **3（192 位）** | 5（256 位） |

**ML-KEM-768** 是推荐选择——它是 X-Wing、Signal 的 PQXDH 以及 Chrome/Firefox
TLS 混合密钥交换所使用的参数集。类别 3 针对未来格密码分析进展提供了
充裕的安全边际。

### 混合方案如何工作

**当前流程（易受攻击）：**

```text
Sender:
  esk = PRF(rseed, rho)                    // deterministic from note
  epk = [esk] * g_d                         // Pallas curve point
  shared_secret = [esk] * pk_d              // ECDH (broken by Shor's)
  K_enc = BLAKE2b("Zcash_OrchardKDF", shared_secret || epk)
  enc_ciphertext = ChaCha20(K_enc, note_plaintext)
```

**混合流程（抗量子）：**

```text
Sender:
  esk = PRF(rseed, rho)                    // unchanged
  epk = [esk] * g_d                         // unchanged
  ss_ecdh = [esk] * pk_d                    // same ECDH

  (ct_pq, ss_pq) = ML-KEM-768.Encaps(ek_pq)  // NEW: lattice-based KEM
                                                // ek_pq from recipient's address

  K_enc = BLAKE2b(                          // MODIFIED: combines both secrets
      "DashPlatform_HybridKDF",
      ss_ecdh || ss_pq || ct_pq || epk
  )

  enc_ciphertext = ChaCha20(K_enc, note_plaintext)  // unchanged
```

**接收方解密：**

```text
Recipient:
  ss_ecdh = [ivk] * epk                    // same ECDH (using incoming viewing key)
  ss_pq = ML-KEM-768.Decaps(dk_pq, ct_pq)  // NEW: KEM decapsulation
  K_enc = BLAKE2b("DashPlatform_HybridKDF", ss_ecdh || ss_pq || ct_pq || epk)
  note_plaintext = ChaCha20.Decrypt(K_enc, enc_ciphertext)
```

### 安全保证

组合 KEM 在**任一**组成 KEM 安全的情况下即为 IND-CCA2 安全。这由
[Giacon、Heuer 和 Poettering（2018）](https://eprint.iacr.org/2018/024)
对使用 PRF 的 KEM 组合器（BLAKE2b 符合条件）正式证明，同时由
[X-Wing 安全性证明](https://eprint.iacr.org/2024/039)独立证明。

| 场景 | ECDH | ML-KEM | 组合密钥 | 状态 |
|------|------|--------|---------|------|
| 经典世界 | 安全 | 安全 | **安全** | 两者完好 |
| 量子破解 ECC | **已破解** | 安全 | **安全** | ML-KEM 保护 |
| 格密码进展破解 ML-KEM | 安全 | **已破解** | **安全** | ECDH 保护（与今天相同） |
| 两者均被破解 | 已破解 | 已破解 | **已破解** | 需要两个同时的突破 |

### 大小影响

混合 KEM 为每个存储的票据添加 ML-KEM-768 密文（1,088 字节），并扩展出站
密文以包含用于发送方恢复的 ML-KEM 共享密钥：

**每个票据的存储记录：**

```text
┌──────────────────────────────────────────────────────────────────┐
│  Current (280 bytes)         Hybrid (1,400 bytes)               │
│                                                                  │
│  cmx:             32         cmx:              32               │
│  rho:             32         rho:              32               │
│  epk:             32         epk:              32               │
│  enc_ciphertext: 104         ct_pq:         1,088  ← NEW       │
│  out_ciphertext:  80         enc_ciphertext:  104               │
│                              out_ciphertext:  112  ← +32        │
│  ─────────────────           ──────────────────────             │
│  Total:          280         Total:          1,400  (5.0x)      │
└──────────────────────────────────────────────────────────────────┘
```

**大规模存储：**

| 票据数量 | 当前（280 B） | 混合（1,400 B） | 增量 |
|---------|-------------|----------------|------|
| 100,000 | 26.7 MB | 133 MB | +106 MB |
| 1,000,000 | 267 MB | 1.33 GB | +1.07 GB |
| 10,000,000 | 2.67 GB | 13.3 GB | +10.7 GB |

**地址大小：**

```text
Current:  diversifier (11) + pk_d (32) = 43 bytes
Hybrid:   diversifier (11) + pk_d (32) + ek_pq (1,184) = 1,227 bytes
```

1,184 字节的 ML-KEM 公钥必须包含在地址中，以便发送方执行封装。以约 1,960 个
Bech32m 字符计，这很大但仍然可以放入 QR 码（最大约 2,953 个字母数字字符）。

### 密钥管理

ML-KEM 密钥对从支出密钥确定性派生：

```text
SpendingKey (sk) [32 bytes]
  |
  +-> ... (all existing Orchard key derivation unchanged)
  |
  +-> ml_kem_d = PRF^expand(sk, [0x09])[0..32]
  +-> ml_kem_z = PRF^expand(sk, [0x0A])[0..32]
        |
        +-> (ek_pq, dk_pq) = ML-KEM-768.KeyGen(d=ml_kem_d, z=ml_kem_z)
              ek_pq: 1,184 bytes (public, included in address)
              dk_pq: 2,400 bytes (private, part of viewing key)
```

**无需更改备份。**现有的 24 个助记词覆盖了 ML-KEM 密钥，因为它是从支出密钥
确定性派生的。钱包恢复照常工作。

**多样化地址**都共享相同的 `ek_pq`，因为 ML-KEM 没有像 Pallas 标量乘法
那样的自然多样化机制。这意味着拥有用户两个地址的观察者可以通过比较 `ek_pq`
将它们关联起来。

### 试解密性能

| 步骤 | 当前 | 混合 | 增量 |
|------|------|------|------|
| Pallas ECDH | ~100 us | ~100 us | — |
| ML-KEM-768 Decaps | — | ~40 us | +40 us |
| BLAKE2b KDF | ~0.5 us | ~1 us | — |
| ChaCha20（52 字节） | ~0.1 us | ~0.1 us | — |
| **每票据总计** | **~101 us** | **~141 us** | **+40% 开销** |

扫描 100,000 个票据：~10.1 秒 → ~14.1 秒。开销显著但不会造成阻碍。ML-KEM
解封装是常数时间的，没有批处理优势（不同于椭圆曲线操作），因此线性扩展。

### 对 ZK 电路的影响

**无影响。**混合 KEM 完全在传输/加密层。Halo 2 电路证明票据存在性、作废符
正确性和价值平衡——不证明任何关于加密的内容。证明密钥、验证密钥或电路约束
无需更改。

### 与业界比较

| 系统 | 方法 | 状态 |
|------|------|------|
| **Signal**（PQXDH） | X25519 + ML-KEM-768，所有用户强制使用 | **已部署**（2023） |
| **Chrome/Firefox TLS** | X25519 + ML-KEM-768 混合密钥交换 | **已部署**（2024） |
| **X-Wing**（IETF 草案） | X25519 + ML-KEM-768，专用组合器 | 标准草案 |
| **Zcash** | 量子可恢复性 ZIP 草案（资金恢复，非加密） | 仅讨论阶段 |
| **Dash Platform** | Pallas ECDH + ML-KEM-768（提议中） | 设计阶段 |

## 何时部署

### 时间线问题

- **当前状态（2026）：**没有量子计算机能破解 255 位 ECC。已展示的最大量子
  分解：约 50 位。差距：数个数量级。
- **近期（2030-2035）：**IBM、Google、Quantinuum 的硬件路线图目标为数百万
  量子比特。ML-KEM 实现和参数集将趋于成熟。
- **中期（2035-2050）：**大多数估计将 CRQC 的到来放在此窗口。今天收集的
  HNDL 数据面临风险。
- **长期（2050+）：**密码学家的共识：大规模量子计算机是"何时"的问题，
  而非"是否"。

### 推荐策略

**1. 现在就为可升级性而设计。**确保存储记录格式、`TransmittedNoteCiphertext`
结构和 BulkAppendTree 条目布局是版本化和可扩展的。成本低，保留了日后添加
混合 KEM 的选项。

**2. 准备就绪时部署混合 KEM，使其成为强制性的。**不要提供两个池（经典和
混合）。分割匿名集违背了隐私交易的目的——在较小群体中隐藏的用户隐私性更低，
而非更高。部署后，每个票据都使用混合方案。

**3. 瞄准 2028-2030 窗口。**这远在任何现实量子威胁之前，但在 ML-KEM 实现
和参数大小稳定之后。这也允许从 Zcash 和 Signal 的部署经验中学习。

**4. 监控触发事件：**
- NIST 或 NSA 强制规定后量子迁移截止日期
- 量子硬件的重大进展（>100,000 个带纠错的物理量子比特）
- 针对格问题的密码分析进展（将影响 ML-KEM 的选择）

### 无需紧急行动的组件

| 组件 | 可以等待的原因 |
|------|--------------|
| 支出授权签名 | 伪造是实时的，非追溯性的。在 CRQC 到来前升级到 ML-DSA/SLH-DSA。 |
| Halo 2 证明系统 | 证明伪造是实时的。需要时迁移到基于 STARK 的系统。 |
| Sinsemilla 碰撞抗性 | 仅对新攻击有用，非追溯性的。被证明系统迁移所包含。 |
| GroveDB Merk/MMR/Blake3 基础设施 | **已经量子安全。**无需任何行动，现在或将来都不需要。 |

## 后量子替代方案参考

### 用于加密（替代 ECDH）

| 方案 | 类型 | 公钥 | 密文 | NIST 类别 | 备注 |
|------|------|------|------|----------|------|
| ML-KEM-768 | 格（MLWE） | 1,184 B | 1,088 B | 3（192 位） | FIPS 203，行业标准 |
| ML-KEM-512 | 格（MLWE） | 800 B | 768 B | 1（128 位） | 更小，安全边际更低 |
| ML-KEM-1024 | 格（MLWE） | 1,568 B | 1,568 B | 5（256 位） | 对混合方案过度 |

### 用于签名（替代 RedPallas/Schnorr）

| 方案 | 类型 | 公钥 | 签名 | NIST 类别 | 备注 |
|------|------|------|------|----------|------|
| ML-DSA-65（Dilithium） | 格 | 1,952 B | 3,293 B | 3 | FIPS 204，快速 |
| SLH-DSA（SPHINCS+） | 基于哈希 | 32-64 B | 7,856-49,856 B | 1-5 | FIPS 205，保守 |
| XMSS/LMS | 基于哈希（有状态） | 60 B | 2,500 B | 可变 | 有状态——重用即破解 |

### 用于 ZK 证明（替代 Halo 2）

| 系统 | 假设 | 证明大小 | 后量子 | 备注 |
|------|------|---------|-------|------|
| STARKs | 哈希函数（碰撞抗性） | ~100-400 KB | **是** | StarkNet 使用 |
| Plonky3 | FRI（基于哈希的多项式承诺） | ~50-200 KB | **是** | 活跃开发中 |
| Halo 2（当前） | Pasta 曲线上的 ECDLP | ~5 KB | **否** | 当前 Orchard 系统 |
| Lattice SNARKs | MLWE | 研究中 | **是** | 未达生产就绪 |

### Rust Crate 生态系统

| Crate | 来源 | FIPS 203 | 已验证 | 备注 |
|-------|------|----------|-------|------|
| `libcrux-ml-kem` | Cryspen | 是 | 形式化验证（hax/F*） | 最高保证 |
| `ml-kem` | RustCrypto | 是 | 常数时间，未审计 | 生态系统兼容性 |
| `fips203` | integritychain | 是 | 常数时间 | 纯 Rust，no_std |

## 总结

```text
┌─────────────────────────────────────────────────────────────────────┐
│  GROVEDB + ORCHARD 量子威胁总结                                      │
│                                                                     │
│  现在和永远安全（基于哈希）：                                          │
│    ✓ Blake3 Merk 树、MMR、BulkAppendTree                            │
│    ✓ BLAKE2b KDF、PRF^expand                                       │
│    ✓ ChaCha20-Poly1305 对称加密                                     │
│    ✓ 所有 GroveDB 证明认证链                                         │
│                                                                     │
│  在数据存储之前修复（追溯性 HNDL）：                                    │
│    ✗ 票据加密（ECDH 密钥协商）→ 混合 KEM                              │
│    ✗ 价值承诺（Pedersen）→ 金额暴露                                   │
│                                                                     │
│  在量子计算机到来之前修复（仅实时）：                                    │
│    ~ 支出授权 → ML-DSA / SLH-DSA                                    │
│    ~ ZK 证明 → STARKs / Plonky3                                    │
│    ~ Sinsemilla → 基于哈希的默克尔树                                  │
│                                                                     │
│  推荐时间线：                                                        │
│    2026-2028：为可升级性而设计，版本化存储格式                          │
│    2028-2030：部署强制性混合 KEM 用于票据加密                          │
│    2035+：如需要则迁移签名和证明系统                                   │
└─────────────────────────────────────────────────────────────────────┘
```

---
