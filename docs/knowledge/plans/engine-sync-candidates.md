---
status: active
id: kb-plan-engine-sync-candidates
kind: plan
scope: repository
read_when: planning the next vendor sync or reviewing the engine delta
last_verified: 2026-09-24
sources: ["vendor/README.md", "docs/knowledge/decisions/0003-selective-upstream-alignment.md", "docs/knowledge/vendor-tokscale.md", "https://github.com/Nanako0129/tokscale-core/blob/d6512f5ae62c2be6751ed93adb9391ffe3f91579/UPSTREAM.md", "https://github.com/junhoyeo/tokscale"]
---

# Next engine-sync candidates

## 文件目的

這份文件是**下一次 vendor sync 的候選清單**，不是已落地的 ledger。它記錄我們 vendored engine 與上游 reviewed pin 之間的 delta、每一項在我們樹上的實測現況、分類與規模，讓下一個里程碑可以直接從這裡選一個 bounded scope。

已落地的內容仍由 [`vendor/README.md`](../../../vendor/README.md) 擁有；本文件只保存「還沒拿、以及為什麼」。

## 基準與方法

| 項目 | 值 |
|---|---|
| 我們的 vendored engine | `vendor/tokscale-core`（in-tree vendored source，非 gitlink） |
| 我們的 baseline | junhoyeo/tokscale `0c820a5d` + local patches + 已落地批次至 **M15**（monolithic cache schema **31**） |
| 上游 reviewed pin | [`Nanako0129/tokscale-core` @ `d6512f5`](https://github.com/Nanako0129/tokscale-core/commit/d6512f5ae62c2be6751ed93adb9391ffe3f91579)（上游 TokenBar `vendor/README.md` 的 Reviewed pin） |
| 上游 pin 的狀態 | 對應上游 ledger 的 **M26-B** checkpoint：shard cache format 2 + per-client parser versions，audited 分類為 `79/0/0/18/13/1`（沒有 pending 的 `TAKE`） |
| pin 是否已落後 | engine repo main 只比 pin 多 **16 個 commit，全部是 chore/docs**（CodeRabbit／Copilot 設定、branch naming），**沒有 runtime 變更** — 以 `d6512f5` 比較即可 |

方法：以上游 `UPSTREAM.md`（引擎自己的 immutable ledger）逐批次讀取，再對我們的樹做**存在性驗證**（grep 具體 code marker），最後依 [ADR 0003](../decisions/0003-selective-upstream-alignment.md) 的 selection matrix 分類。**每一列都必須在下手前做 hunk-level diff**：這裡的「我們的現況」是 marker 級證據，不是逐 hunk 結論。

## 候選清單

### A. 可立即拿的 correctness（小、低風險）

> **S1 已於 2026-09-24 落地為 M16 batch**（見 [`vendor/README.md`](../../../vendor/README.md)）：以下四列全部納入，monolithic cache schema 31→32。表格保留為當時的判定與驗證方式記錄。

| 候選 | 內容 | 我們的現況（已驗證） | 分類 | Schema | 規模 |
|---|---|---|---|---|---|
| Pricing fallback tie determinism | `PricingLookup` 對 LiteLLM／OpenRouter 等長 key 只按長度排序，同長度會落回 `HashMap` 迭代順序 → 同一份 catalog 重建可能選到不同 fallback 費率，重算歷史成本會跳動。上游補上 lexicographic secondary order。 | `pricing/lookup.rs` 只有 `sort_by_key(Reverse(len))`，**缺** secondary tie-break | LANDED（M16） | 無（report-time lookup） | S |
| `#1037`（`275bc798`） | Claude `tool_result` 的 input tokens 不再用字元數估算（Claude Code 不寫該 metadata，而下一個 assistant turn 已經回報同一段文字 → 重複計算）。 | `sessions/claudecode.rs` 仍有 `tool_result_output_char_count` 與 char-estimate 測試 → **缺** | LANDED（M16） | 31→32（parser 輸出改變） | S |
| M16 剩餘 parser correctness（`b59979c5` #892、`18cd13cc` #891） | Claude bare transcript（`~/.claude/transcripts/`）不再合成 char-estimated tool-result tokens；Claude request timestamp 留在 activity start，重複 chunk 取 per-field token 最大值與最長 duration；Copilot 偏好 OTEL `startTime`、end-only 記錄反推、重複 span identity 摺疊（per-bucket maxima／最早 start／最長 duration／缺 agent 補回）；Jcode 以 `tool_duration_ms` 反推 turn start。 | 我們在 M15 只拿了 `#890`／`#896`；`is_bare_transcript` marker **不存在**，`copilot.rs` 只有部分 `startTime` 讀取，`jcode.rs` 只把 `tool_duration_ms` 寫進 `duration_ms`、沒有反推 start | LANDED（M16；Claude 以最終 upstream 形狀落地，`#891` 實為 Antigravity alias、`#898` 的 kiro hunk 歸 M15-B） | 31→32 | M |
| M16 provider hardening（`34cfbb50` #887 hunks） | Kimi→`moonshotai`、MiMo→`xiaomi`、GLM→`zai`；GJC／Pi 缺 provider 時先由 model 推斷再退回 client；Antigravity IDE placeholder 與 CLI response-model id 走 single-hop machine alias（含 Gemini 3.5 Flash High 對應）。 | `provider_identity.rs` 已有 `zai`／`moonshotai`，**沒有 `xiaomi`**；Antigravity alias 未驗證 | LANDED（M16；只取 provider hunks，9Router bridge／scanner 留 `DEFER`；Antigravity alias 另行隨 `#891` 落地） | 31→32（provider 影響 attribution／pricing） | S–M |

### B. 中等：新來源與定價

| 候選 | 內容 | 我們的現況（已驗證） | 分類 | Schema | 規模 |
|---|---|---|---|---|---|
| M18 定價 | Sakana `fugu-ultra` 費率與 identity gate；**whole-request** long-context 規則（`input + cache_read > 272,000` 才切 tier，cache-write 不觸發，output+reasoning 跟隨選定 tier）；routed prefix／suffix 組合；forced source 大小寫不敏感；provider-scoped path 不外溢。 | `above_272k` tier **機制已存在**（`lookup.rs` 19 處引用），但 whole-request 規則與 forced-source 大小寫處理**未驗證**；Sakana／Fugu 未見 | **PARTIAL（M18）**：LiteLLM whole-request 規則 + forced-source 大小寫已落地；Sakana catalog 維度與 routed precedence 細修未取（見 D 區） | 無（pricing 在 cache 之後） | M–L |
| M15-A Kiro IDE globalStorage（macOS） | Kiro IDE `~/Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent`（與小寫變體）的 `.chat`／execution／workspace-session 三種 layout，含 suppression 與 cohort 隔離。 | `scanner.rs` 的 `globalStorage` 只指向 `saoudrizwan.claude-dev`（Cline），**Kiro 的沒有** | TAKE（macOS-only，符合本產品） | 無（新來源） | M |
| M15-B Kiro structured sessions | `~/.kiro/sessions/<workspace>/sess_*/session.json` + sibling `messages.jsonl`（parser dependency），`contextUsage` 估 input、`usage_summary.elapsedTime` 補 duration。 | 未見 | TAKE | 無（新來源） | M |
| M25 reloadable model aliases | 新檔 `model_alias.rs`：`{alias → canonical}` 只在**報表分組**終端折疊；pricing 與 `canonical_model_id` 走原始 id。上游把它做成可 reload + invalidation hook。 | `model_alias.rs` **不存在** | TAKE（core API 先拿，Swift 設定面另議） | 無 | S–M |
| M21 Kimi Code | 沿用 public client id `kimi`，以 `sessions/<workspace>/<session>/agents/<agent>/wire.jsonl` topology 選取，只計 `usage.record`；預設 `~/.kimi-code` 與 `KIMI_CODE_HOME`，與 legacy `~/.kimi` 並存。 | `scanner.rs` 沒有 `agents/` topology；`lib.rs` 的 `wire.jsonl` 是 legacy Kimi 路徑 | TAKE（沿用既有 client id，不是新 client） | 無（新來源） | M |

### C. 大／需要明確決策

| 候選 | 內容 | 我們的現況（已驗證） | 分類 | Schema | 規模 |
|---|---|---|---|---|---|
| M17 Grok unified log | 新增 exact top-level `$GROK_HOME/logs/unified.jsonl` 來源；`shell.turn.inference_done` 拆成互斥 bucket；一個純 selector 決定 unified／legacy authority（session-scoped，衝突 fail closed）；FFI live-tail 需帶 reasoning bucket 與 parser message count。上游明確**保留**我們這種 legacy compaction counter-epoch 硬化。 | 全樹沒有 `unified.jsonl` → **完全缺** | 需決策（若實際 Grok 已寫 unified log，價值高；否則只是新來源） | 無（新來源；上游當時維持 31） | L |
| RET-CLAUDE-001 Claude retention | Claude in-place transcript rewrite（compaction）不再遺失歷史：`retained_keys` + 跨檔 dedup 兩段式（live 優先）+ save-merge 修復。代價是 **cache 成為被丟棄 turn 的唯一副本** — 之後每一次 Claude parser 變更都是資料遺失決策，不是單純 invalidation。 | `retain_observed_messages`／`retained_keys` marker **不存在** | 需決策（價值高、語意重） | 需 cache format／schema 決策 | L |
| M23-D Copilot Desktop | 新來源 `~/.copilot/data.db`（只讀 token-bearing `sessions` rows）+ `session-state/*/events.jsonl` 補 model／workspace + OTEL whole-session authority。 | `sessions/copilot_desktop.rs` **不存在** | 需決策（新來源，屬已出貨的 copilot 家族） | 無（新來源，上游維持 32） | L |
| M21 Junie／OpenCodeReview | 兩個新 client（`ClientId::Junie = 31`、`OpenCodeReview = 32`）。 | `sessions/junie.rs`／`opencodereview.rs` **不存在** | 需決策（[alignment plan](tokscale-alignment.md) 的「new client breadth = deferred」） | 無 | M |

### D. 明確跳過或維持 defer

> **本地證據（2026-09-24）：** 下列候選在本產品線上沒有資料可驗證，因此維持跳過。要重新評估就重跑這些檢查。
>
> | 候選 | 證據 |
> |---|---|
> | M17 Grok unified log | 本機 `~/.grok/logs/unified.jsonl` 存在但 107 筆全是 startup／subscription／model-catalog 噪音，`inference_done` **0 筆**；Grok 用量來自 legacy `updates.jsonl`（我們已支援並帶本地 compaction-epoch 硬化）。mini 沒有 `~/.grok/logs`。 |
> | M15-A／M15-B Kiro | 本機與 mini 均無 `~/.kiro`、無 `kiro-cli`。 |
> | M21 Kimi Code | 本機無 `~/.kimi-code`；mini 有但只有 4.0K（設定目錄，無 session）。 |
> | M21 Junie／OpenCodeReview | 本機與 mini 均無 `~/.junie`。 |
> | M23-D Copilot Desktop | 本機與 mini 均無 `~/.copilot/data.db`。 |
> | M25 model aliases | 上游只落了 core API（Swift 設定面自己也沒接）；我們沒有 alias 設定入口，port 進來是 inert 程式碼。 |
> | M18 Sakana catalog | 需要新增一個 pricing source 維度並穿過每條 lookup 路徑，換取一條沒有用量的模型線。 |
> | M18 routed precedence 細修 | 未取；我們已有 #832／#1029 的 routed prefix fallback。若日後出現 `accounts/<provider>/...` 或括號 suffix 的實際誤價案例再評估。 |

| 候選 | 處置 | 理由 |
|---|---|---|
| M26-A／M26-B shard cache | DEFER | 架構替換（256 shards、`CACHE_FORMAT_VERSION`、per-client parser versions），與我們的 monolithic schema 31 + `HASH_MEMO`／`STORE_MEMO` + `modified_after` pruning 正面衝突；上游自己也說 shard 之後 per-client parser version 變成 append-only。要拿就必須是一個獨立的架構里程碑，不能夾在 correctness batch 裡。 |
| M19-A Windows atomic replacement | SKIP | macOS-only 產品。 |
| M23-H Hermes Windows discovery | SKIP | 同上；macOS／profile 那一半已在 M11 落地。 |
| M22 Zcode、M23-V Copilot VS Code `chatSessions`、M24 Warp、Devin、Command Code、CodeBuddy、9Router bridge | DEFER／SKIP | 上游自己的 fidelity stop 或 `DEFER`；我們沒有這些 client，或上游語意尚未收斂。 |

## 建議的里程碑切法

| 里程碑 | Scope | 為什麼這樣切 |
|---|---|---|
| **S1（建議先做）** | A 區全部（pricing tie determinism + `#1037` + M16 剩餘 parser + provider hardening） | 全部是「現有 parser 的錯誤輸出」，符合維護期 priority 1–2；一次 schema bump 31→32 就涵蓋，且不需要動 streaming lane 結構。 |
| **S2** | B 區剩餘（Kiro IDE globalStorage、Kiro structured、M25 aliases、Kimi Code）＋ M18 的 Sakana／routed 細修 | 依 2026-09-24 本地證據全部為 **skip/defer**；重開前先重驗資料是否存在 | |
| **S3** | C 區逐項決策（Grok unified、Copilot Desktop、Claude retention） | 每一項都需要產品／語意決策，尤其 retention 會改變 cache 的責任。2026-09-24 起 Grok unified 與 Copilot Desktop 已由本地證據判定無資料；**retention 是唯一剩下有實質價值的項目**。 |

> **順序警告：** 若未來要拿 RET-CLAUDE-001，應先決定它、再拿任何 Claude parser 修正。上游 ledger 明確記錄：retention 之後每一個 Claude parser 變更都變成資料遺失決策，所以他們把 `#1037` 這種小修「等來源下次自然變更」才生效。我們現在還沒有 retention，所以 A 區的 Claude 修正可以乾淨地拿。

## 驗證要求

每一列都適用 [`verification.md`](../verification.md) 與 [ADR 0003](../decisions/0003-selective-upstream-alignment.md) 的規則：

| 面向 | 要求 |
|---|---|
| 基線 | 先 diff 實際 hunk，不以 commit 標題判斷（`#760`／`#662` 都是標題低估 runtime 變更的前例） |
| Fixture | 一個 old-fail／new-pass 的 hermetic fixture |
| Cache | parser 輸出／dedup key／attribution 有變就 bump 我們的 monolithic schema，並證明同 fingerprint 的舊 entry 會被拒並重建 |
| Streaming | 我們有 local streaming lane：`simple_lane!`、fingerprint、mtime probe、prune 都要一起改，並驗 materialized／streaming 對齊 |
| 邊界 | 若動到 report options 或 payload，同步檢查 C ABI 與 Swift decoder |
| 授權 | 本文件不授權 push、merge、tag 或 release |

## 證據

| 主張 | 證據入口 |
|---|---|
| 上游批次、commit 與分類 | `tokscale-core` `UPSTREAM.md` @ `d6512f5`（本文件 sources 的 URL） |
| pin 之後只有 chore/docs | engine repo `d6512f5..main` 的 16 個 commit 標題 |
| 我們已落地的批次與 local patches | [`vendor/README.md`](../../../vendor/README.md) |
| 我們的 cache 形狀（monolithic schema 31） | `vendor/tokscale-core/src/message_cache.rs` |
| 各項 marker 的存在性 | 本文件各列的「我們的現況」欄（grep 級證據；下手前仍需 hunk-level diff） |
