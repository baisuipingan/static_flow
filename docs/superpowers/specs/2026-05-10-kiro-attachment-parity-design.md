# Kiro Attachment Protocol Parity Design

## Goal

在不破坏现有 userspace 请求结构的前提下，把当前 `static_flow` 的 Kiro 图片/PDF/文档处理链路重构为**与官方 Kiro IDE 一致的附件协议语义**：

- 图片按原始二进制附件进入 upstream `images`
- PDF/文档按原始二进制附件进入 upstream `documents`
- 不再把 PDF 先抽文本再塞回 `content`
- 附件数量、去重和格式边界与官方 bundle 已确认行为对齐

本次设计的重点不是“让更多请求碰巧过”，而是把当前错误的本地语义改回正确的一般机制。

## Scope

### In Scope

- 重构 `llm-access-kiro` 内部的图片/文档附件表示
- 删除 PDF 抽文本路径，改成原始文档附件透传
- 在 `llm-access` Kiro upstream builder 中显式构造 `images` / `documents`
- 对齐官方已确认的附件数量限制与会话级文档去重
- 放宽远程 URL 文档归一化，使其能进入与本地附件相同的内部模型
- 增加针对图片、PDF、Office 文档和 body/数量限制的回归测试

### Out of Scope

- 改动 Kiro upstream URL、headers、auth refresh、usage 刷新逻辑
- 改动 Anthropic-facing public request schema
- 改动 thinking / effort 语义
- 追求对官方 IDE 的字节级请求完全复制
- 添加新的兼容分支去保留“PDF 抽文本”旧行为

## Confirmed Findings

### 1. 官方 Kiro IDE 使用原始附件协议，而不是 PDF 文本提取

官方 bundle 的聊天路径会把：

- 图片收集为 `imageBase64Urls`
- 文档收集为 `documentAttachments`

之后分别转成 upstream `images[*].source.bytes` 和 `documents[*].source.bytes`。这条链路在 bundle 内是显式的，而不是通过 prompt 拼接实现。

### 2. 当前本地实现与官方最大偏差在 PDF

本地 `normalize_user_document_block(...)` 当前会：

- 只接受 `application/pdf`
- 用 `lopdf` 提取文本
- 生成 `PDF extracted text:` 文本块

这意味着 PDF 已经不再是附件，而变成了 prompt 内容，协议语义与官方不同。

### 3. 当前 `wire.rs` 只有图片附件，没有文档附件

`llm-access-kiro/src/wire.rs` 的 `UserInputMessage` / `UserMessage` 只持有 `images`，没有 `documents` 字段。这导致即使前置层解析出文档，也没有原生落点承接到 upstream 请求结构。

### 4. 官方已确认的附件限制比我们当前更像“协议规则”

从官方 bundle 已经确认：

- 单次消息最多 10 张图片
- 单次消息最多 5 个文档附件
- 文档名会在消息内和会话内去重
- 超过 5 个文档会直接拒绝

这些限制属于“协议层”的清晰边界。相对地，我们当前最显眼的是本地 1.6MB 总 body 限制，它更像服务保护，而不是官方协议语义。

### 5. 当前远程 URL 文档支持过窄

我们现在远程 URL 文档路径只支持：

- `application/pdf`
- `text/plain`

但官方前端允许的文档集合更宽：

- `pdf`
- `csv`
- `doc`
- `docx`
- `xls`
- `xlsx`
- `html`
- `txt`
- `md`

### 6. 图片格式边界要按 runtime 能力，不按 UI 识别集合

官方 UI 文件识别集合里出现了 `image/svg+xml`，但 runtime `ImageFormat` 显式映射只确认了：

- PNG
- JPEG
- GIF
- WEBP

本轮必须按 runtime 真实可编码格式收口，而不是因为前端能识别就一并放开。

## Constraints

- 不破坏现有 public Anthropic 请求形状
- 不引入“旧 PDF 抽文本”和“新原始附件”并存的长期双语义
- 不把服务保护阈值伪装成官方协议规则
- 所有限制都必须来自已确认的官方行为或明确的服务端保护目的

## Options Considered

### Option A: 保留 PDF 抽文本，只补附件数量和 MIME

优点：

- 改动小

缺点：

- 核心协议仍是错的
- 后续所有“为什么官方能过、我们不过”的问题还会继续出现

### Option B: 做协议对齐式重构，但保持 userspace 输入兼容

优点：

- 命中真正根因
- userspace 无需迁移
- 后续维护边界清晰

缺点：

- 会跨 `llm-access-kiro` 与 `llm-access` 两层改动

### Option C: 直接照搬官方完整内部 message builder

优点：

- 最接近官方实现

缺点：

- 侵入性太大
- 超出当前需求

## Chosen Design

采用 **Option B**。

核心原则：

1. **外部兼容，内部重写**
   - 对外仍接受现在的 Anthropic 风格图片/文档请求
   - 对内统一转换成“官方等价附件模型”

2. **删除错误语义，不做双轨保留**
   - 不再保留 PDF 抽文本作为常规路径
   - PDF/文档一律进入 upstream `documents`

3. **把协议限制和服务保护限制拆开**
   - 附件数量、去重、格式属于协议层
   - 总字节保护如果保留，必须明确是服务保护层

## Detailed Design

### 1. 在 `llm-access-kiro` 增加一等文档附件模型

目标文件：

- `llm-access-kiro/src/wire.rs`
- `llm-access-kiro/src/anthropic/converter.rs`

设计：

- 为当前 Kiro wire 类型补 `documents`
- 文档结构与官方 bundle 对齐：
  - `name`
  - `format`
  - `source.bytes`
- 图片继续保留 `format + source.bytes`
- `UserInputMessage` 和历史 `UserMessage` 都要能承接 `documents`

理由：

- 只有 `images` 没有 `documents` 的 wire 不足以表达官方协议
- 这一步是后续 builder 对齐的基础

### 2. 删除 PDF 文本提取路径

目标文件：

- `llm-access-kiro/src/anthropic/converter.rs`

设计：

- 删除 `normalize_user_document_block(...)` 中对 PDF 的文本提取
- 删除 `extract_pdf_document_text(...)` 及其 xref repair 辅助逻辑
- 文档块在 normalize 阶段只做结构验证和标准化，不再改写成 text block

新的 normalize 规则：

- `document` block 必须有 `source`
- `source.type` 允许：
  - `base64`
  - `text` 仅用于 plain text / markdown / html / csv 这类文本文档的直传表示
- 二进制文档不再被转换成 user text

### 3. 明确区分“文本内容”和“附件内容”

目标文件：

- `llm-access-kiro/src/anthropic/converter.rs`
- `llm-access-kiro/src/wire.rs`

设计：

- `content` 只承载真正文本
- 图片和文档都从 `content` 的 typed blocks 分离为附件集合
- converter 输出同时返回：
  - 规范化文本
  - 图片附件
  - 文档附件

这样 `provider.rs` 不再需要从拼接后的文本里倒推附件语义。

### 4. 在 `provider.rs` 中按官方协议生成 upstream 请求

目标文件：

- `llm-access/src/provider.rs`

设计：

- 构造 `ConversationState.current_message.user_input_message` 时：
  - 文本进入 `content`
  - 图片进入 `images`
  - 文档进入 `documents`
- 历史消息若带附件，也要在对应历史 user message 中保留 `images/documents`

同时对齐官方附件规则：

- 图片最多 10 张
- 文档最多 5 个
- 文档名去重
- 会话内文档总量超过 5 直接本地拒绝

这里的去重必须是稳定、确定性的：

- 同消息内重名文档丢弃后者并记录日志
- 跨会话历史重名文档同样丢弃后者并记录日志

### 5. 远程 URL 附件进入同一内部模型

目标文件：

- `llm-access/src/provider.rs`

设计：

- 保留现在的 URL source 能力，避免破坏 userspace
- 但 URL 拉取结果必须归一化为与本地附件一致的内部附件模型

远程文档白名单扩展到：

- `application/pdf`
- `text/csv`
- `application/msword`
- `application/vnd.openxmlformats-officedocument.wordprocessingml.document`
- `application/vnd.ms-excel`
- `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
- `text/html`
- `text/plain`
- `text/markdown`

远程图片仍限制为：

- `image/jpeg`
- `image/png`
- `image/gif`
- `image/webp`

原因：

- 这是官方 runtime 已确认能编码的格式集合
- SVG 不在本轮放开范围内

### 6. 总 body 限制降级为“服务保护层”

目标文件：

- `llm-access/src/provider.rs`

设计：

- 当前 `KIRO_GENERATE_REQUEST_MAX_BODY_BYTES` 不再作为协议主约束
- 若保留总字节限制，应改名并改语义，明确为：
  - 服务端保护
  - 防止异常超大请求压垮本地 gateway

这条限制不能再参与“官方协议解释”。对外错误信息也应表明这是本地服务保护，而不是 upstream 协议限制。

### 7. 日志与错误语义

目标文件：

- `llm-access/src/provider.rs`
- `llm-access-kiro/src/anthropic/converter.rs`

设计：

- 对附件数量超限、重名去重、格式非法、本地服务保护超限分别打结构化日志
- 保留 upstream `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 到用户错误的映射
- 本地拒绝与 upstream 拒绝必须可区分

## Backward Compatibility

保留兼容：

- 现有 public request shape 不变
- 现有 URL source 请求方式不变
- 现有图片请求方式不变

有意改变：

- PDF 不再被转换成 prompt 文本
- 文档不再作为“文本增强”的隐式机制
- 某些以前被 1.6MB 本地阈值提前拦掉、但符合官方附件语义的请求，将不再被同样方式拒绝

## Testing Strategy

### Unit Tests

至少覆盖：

1. PDF document block 保持为文档附件，不再改写为 text
2. `pdf/csv/doc/docx/xls/xlsx/html/txt/md` 能正确归一化为文档附件
3. `jpeg/png/gif/webp` 能正确归一化为图片附件
4. 超过 10 张图片会被拒绝
5. 超过 5 个文档会被拒绝
6. 同消息和跨历史消息的重名文档会被去重
7. URL 文档源能进入与本地附件一致的内部模型
8. upstream `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 仍能保留原有用户错误映射

### Integration / Live Verification

使用真实 Kiro auth 和指定代理做验证：

- 代理：`http://127.0.0.1:11114`
- 用真实 PNG/JPEG/GIF/WebP 和真实 PDF/CSV/DOCX/HTML/TXT 样本做直连上游验证
- 对比请求体，确认当前消息与历史消息中都出现 `images/documents`

## Risks

### 1. 模型输出行为会变化

从“PDF 抽文本”切回“原始文档附件”后，模型回复风格可能变化。这是预期中的协议修正，不应被当作回归。

### 2. Office 文档的 URL 解析边界更宽

白名单放宽后，需要保证：

- MIME 识别稳定
- 二进制内容不被误当成文本

### 3. 历史消息附件保留会放大请求体

这会逼近 upstream 自身的输入阈值。因此本地必须把“协议限制”和“服务保护限制”拆开记录，避免再次混淆。

## Rollout

1. 先完成本地单元测试与 crate 级验证
2. 再用真实 auth + `11114` 做 live probe
3. 通过后再提交和发布
4. 上线后再用真实 PDF/图片样本在 public Kiro 路径复核一次
