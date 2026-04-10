# Deep-Dive Technical Analysis: Headroom & RTK Compression Techniques

## Executive Summary

This document provides detailed technical specifications for the compression systems in:
- **Headroom** (Python): Multi-stage compression pipeline with SmartCrusher (JSON), CodeCompressor (AST), and Kompress (NLP)
- **RTK** (Rust): CLI output filtering with 12 distinct strategies achieving 60-99% token savings

---

## PART 1: HEADROOM COMPRESSION PIPELINE

### 1. SmartCrusher (JSON Compression)

**File**: `/tmp/headroom/headroom/transforms/smart_crusher.py` (Lines 314-3621)

#### Core Algorithm Architecture

SmartCrusher uses **5-stage intelligent compression** for JSON arrays:

##### Stage 1: Array Classification
- **Function**: `_classify_array()` (Lines 337-364)
- **Output**: `ArrayType` enum with 7 types:
  - DICT_ARRAY (objects)
  - STRING_ARRAY (strings)
  - NUMBER_ARRAY (numbers)
  - BOOL_ARRAY (booleans)
  - NESTED_ARRAY (arrays)
  - MIXED_ARRAY (multiple types)
  - EMPTY
- **Method**: Single pass through all elements, checking `type()` (O(1) per element)
- **Special**: Handles Python bool-as-int subclass by checking bool first

##### Stage 2: Statistical Field Detection
No hardcoded string patterns. Instead, uses **3 statistical detection methods**:

###### ID Field Detection
- **Function**: `_detect_id_field_statistically()` (Lines 480-526)
- **Rules**:
  - Uniqueness ratio > 0.9 (required)
  - UUID format (structural check: 8-4-4-4-12 hex groups)
  - Sequential numeric pattern (using `_detect_sequential_pattern()`)
  - High entropy random strings (> 0.7 entropy, > 0.95 uniqueness)
  - Value range > 0 with > 0.95 uniqueness
  - Very high uniqueness alone (> 0.98) scores 0.7 confidence
- **Confidence scoring**: 0.7-0.95 range

###### Score/Ranking Field Detection
- **Function**: `_detect_score_field_statistically()` (Lines 529-600)
- **Rules**:
  - Must be numeric type
  - Bounded range detection: [0,1], [0,10], [0,100], [-1,1], [0,5]
  - NOT sequential (unlike IDs)
  - Descending sort detection (> 70% descending pairs)
  - Floating-point prevalence (> 30% floats)
- **Confidence accumulation**: 0.4-0.95 range

###### Outlier Detection
- **Function**: `_detect_structural_outliers()` (Lines 603-647)
- **Method**: Analyzes field count variance, field type variance, and JSON depth
- **Markers**: Items with unusual field sets or deeply nested structures

##### Stage 3: Data Pattern Recognition
- **Function**: `_analyze_array()` (Lines 963-1011)
- **Extracts**:
  - Per-field statistics: type, uniqueness ratio, min/max values, entropy
  - Change points in numeric fields (sudden value shifts)
  - Detected pattern (time-series, logs, search results, generic)
  - Compression crushability assessment

##### Stage 4: Strategy Selection
- **Function**: `_select_strategy()` (Lines 1429-1463)
- **Returns**: One of 6 strategies:

| Strategy | When Used | Keeps |
|----------|-----------|-------|
| **NONE** | Array too small/diverse | All items |
| **SKIP** | Data unsafe to compress | All items |
| **TIME_SERIES** | Temporal data detected | Change points + anomalies + recency |
| **CLUSTER_SAMPLE** | Similar repeated items | Unique items + diversity sample |
| **TOP_N** | Scored/ranked data | Highest-scoring N items + boundaries |
| **SMART_SAMPLE** | Generic/unknown patterns | Adaptive K from information theory |

##### Stage 5: Intelligent Item Selection
Multiple preservation mechanisms (all **additive**, never dropped):

###### Adaptive K Calculation
- **Function**: `_crush_array()` → `compute_optimal_k()` (Lines 2418-2429)
- **Method**:
  1. Kneedle algorithm on bigram coverage curves
  2. Finds elbow point where adding items stops providing new information
  3. Defaults: min_k=3, max_k from config
  4. SmartCrusher never keeps fewer than 3 items
  5. Distribution: 30% from start + 15% from end + 55% for scored items

###### Error Item Preservation
- **Function**: `_detect_error_items_for_preservation()` (Lines 708-745)
- **Method**: Keyword detection for error indicators
- **Preserved keywords**: "error", "exception", "failed", "critical", "fatal", "panic", "failed"
- **Behavior**: ALWAYS preserved regardless of strategy or K budget

###### Semantic Preservation via Anchors
- **Function**: `_plan_time_series()` → `select_anchors()` (Lines 3215-3222)
- **Class**: `AnchorSelector` (File: `/tmp/headroom/headroom/transforms/anchor_selector.py`)
- **Patterns**:
  - **SEARCH_RESULTS**: 50% front (relevant), 10% middle, 40% back
  - **LOGS**: 20% front, 10% middle, 70% back (recency)
  - **TIME_SERIES**: 33% each (balanced temporal)
  - **GENERIC**: 25% each (even distribution)
- **Information scoring**: Uniqueness + content length + structural rarity

###### Query Anchor Matching
- **Function**: `extract_query_anchors()` + `item_matches_anchors()` (Lines 100-152)
- **Method**: Extract exact entity anchors from query (UUIDs, numbers, names)
- **Match**: Items containing same anchors preserved (deterministic exact match)

###### Relevance Scoring
- **Function**: `_plan_time_series()` (Lines 3259-3262)
- **Scorer**: RelevanceScorer (BM25 tier by default)
- **Threshold**: Items with score >= `_relevance_threshold` preserved
- **Scorer initialization**: Line 1543-1550

###### TOIN Learned Preservation
- **Function**: `_plan_time_series()` (Lines 3264-3269)
- **Source**: Tool Output Intelligence Network
- **Method**: SHA256[:8] hashes of field names users commonly retrieve
- **Behavior**: Boosts items where query matches TOIN-learned fields

###### Anomaly Preservation
- **Function**: `_detect_structural_outliers()` (Lines 603-647)
- **Method**: Statistical outlier detection (> 2σ from mean)
- **Types**:
  - Numeric anomalies
  - String length anomalies
  - Change points in temporal data

#### Cross-Cutting Concerns

##### Feedback-Aware Compression
- **Source**: `CompressionFeedback` system tracks item retrieval rates
- **Used in**: Line 2439-2443
- **Effect**: Reduces aggressiveness if users frequently retrieve compressed items

##### TOIN (Tool Output Intelligence Network)
- **Lines**: 2445-2495
- **Inputs**: Tool signature + query context
- **Outputs**:
  - `skip_compression`: Boolean skip flag
  - `max_items`: Override K based on learned patterns
  - `preserve_fields`: Hashed field names to preserve
  - `compression_level`: "conservative"/"moderate"/"aggressive"
- **Confidence threshold**: Line 1471 (defaults to 0.5)
- **Precedence**: Takes priority over local feedback if confidence >= threshold

##### CCR (Compression Cache Retrieval)
- **Enabled when**: Compression ratio < 0.8 (20%+ reduction)
- **Storage**: Hash-based retrieval of original content
- **TTL**: Configurable (default from config)

#### Array-Type-Specific Compression

##### String Arrays
- **Function**: `_crush_string_array()` (Lines 2711-2792)
- **Method**:
  1. Deduplication (unique values only)
  2. Adaptive sampling based on diversity
  3. Error item preservation (strings containing error keywords)
  4. Length-based anomaly detection
- **Handles**: Common string arrays (file paths, tags, log lines)

##### Number Arrays
- **Function**: `_crush_number_array()` (Lines 2794-2900)
- **Method**:
  1. Statistical summary (min, max, mean, median, std dev)
  2. Change point detection (for time series)
  3. Anomaly preservation (> 2σ)
  4. Distinct value clustering
- **Filters**: NaN/Infinity before statistics (isfinite() checks)

##### Mixed Arrays
- **Function**: `_crush_mixed_array()` (Lines 2902-3001)
- **Method**:
  1. Group by type (dict, string, number, bool)
  2. Compress each group independently
  3. Preserve inter-group structure
  4. Apply separate K to each group

##### Object Arrays (Most Complex)
- **Field analysis**: Lines 1013-1090 analyze per-field statistics
- **Constant field factoring**: Lines 1134-1135 (if enabled)
- **Pattern inference**: Time series, logs, etc.

#### Compression Planning Functions

##### Plan: Time Series
- **Function**: `_plan_time_series()` (Lines 3188-3275)
- **Preserves**:
  1. Dynamic anchors (via AnchorSelector)
  2. Change point windows (±2 around each change)
  3. Structural outliers
  4. Error items (keyword-based)
  5. Query-anchored items (exact match)
  6. High-relevance items (scorer)
  7. TOIN-learned fields

##### Plan: Cluster Sample
- **Function**: `_plan_cluster_sample()` (Lines 3277-3381)
- **Method**:
  1. Deduplication via content hashing
  2. Unique cluster detection (SimHash)
  3. Representative sampling from each cluster
  4. Boundary preservation

##### Plan: Top N
- **Function**: `_plan_top_n()` (Lines 3383-3495)
- **Method**:
  1. Score items by relevance/importance
  2. Keep top K by score
  3. Add boundary items (first/last)
  4. Always preserve anomalies

##### Plan: Smart Sample
- **Function**: `_plan_smart_sample()` (Lines 3497-3603)
- **Method**:
  1. Apply information-theoretic K
  2. Deterministic random sampling
  3. Preserve anomalies separately
  4. Seed reproducibility

#### Configuration

**Class**: `SmartCrusherConfig` (headroom/config.py)
- `min_items_to_analyze`: Minimum array items before compression (default: 5)
- `min_tokens_to_crush`: Minimum token count (default: 200)
- `max_items_after_crush`: Maximum items kept after compression
- `factor_out_constants`: Extract repeated field values
- `toin_confidence_threshold`: TOIN override threshold (default: 0.5)
- `enable_ccr`: Enable compression cache retrieval
- `anchor`: AnchorConfig for dynamic anchor selection

---

### 2. CodeCompressor (AST-Aware Code Compression)

**File**: `/tmp/headroom/headroom/transforms/code_compressor.py` (Lines 899-1076)

#### Supported Languages
- Rust, Python, JavaScript, TypeScript, Go, Java, C, C++
- **Uses**: tree-sitter (not bundled, requires `pip install headroom-ai[code]`)

#### Compression Pipeline

##### Language Detection
- **Function**: `detect_language()` (Lines 499-584)
- **Method**: File extension + heuristic analysis (byte patterns, keywords)
- **Returns**: (CodeLanguage, confidence: 0.0-1.0)

##### Symbol Importance Analysis
- **Function**: `_analyze_symbol_importance()` (Lines 685-844)
- **Analyzes**:
  - Export status (public vs private)
  - Definition location (top-level vs nested)
  - Reference density (how often used)
  - Docstring presence
- **Outputs**: Symbol scores dict (name → importance_score)

##### Budget Allocation
- **Function**: `_allocate_body_budget()` (Lines 846-893)
- **Method**: Distribute available tokens across function/method bodies
- **Per-symbol**: Importance score determines allocated token budget
- **Output**: Dict of symbol_name → max_body_tokens

##### AST Extraction
- **Function**: `_compress_with_ast()` (Lines 1078-1127)
- **Method**:
  1. Parse code with tree-sitter
  2. Analyze symbol importance
  3. Allocate compression budget per symbol
  4. Extract structure with budget constraints
  5. Assemble compressed code
- **Returns**: (compressed_code, CodeStructure, symbol_scores)

##### Structure Extraction
- **Function**: `_extract_structure()` (Lines 1133-1260)
- **Visitor pattern** traverses AST, extracts:
  - **imports**: All import statements (preserved fully)
  - **function_signatures**: Function defs (truncated bodies per budget)
  - **class_definitions**: Class defs (truncated methods)
  - **type_definitions**: Type/interface/trait definitions
  - **top_level_code**: Module-level assignments, if __name__ blocks
- **Data-driven**: LangConfig tables specify node types per language
- **Byte range tracking**: Ensures no content duplication

##### Function Body Compression
- **Function**: `_compress_function_ast()` (Lines 1266-1457)
- **Applies to**: Functions, methods, lambdas
- **Truncation rules**:
  1. Keep function signature (full)
  2. Keep first N statements (up to budget)
  3. Add "... [X lines omitted]" comment
  4. Verify syntax validity
- **Budget source**: `_allocate_body_budget()` output

##### Class Compression
- **Function**: `_compress_class_ast()` (Lines 1459-1563)
- **Method**:
  1. Keep class header and property declarations
  2. Compress each method individually
  3. Preserve inheritance chain
  4. Keep docstrings

##### Syntax Validation
- **Function**: `_verify_syntax()` (Lines 1618-1629)
- **Checks**:
  - No ERROR nodes (parse failures)
  - No MISSING nodes (incomplete constructs)
- **Fallback**: Returns original code if invalid

##### Aggressive Compression Guard
- **Lines**: 1009-1023
- **Condition**: `if ratio < 0.05` (less than 5% remaining)
- **Action**: Returns original code (prevents over-compression)

#### Configuration

**Class**: `CodeCompressorConfig`
- `min_tokens_for_compression`: Skip compression if content smaller (default: 50)
- `language_hint`: Force language detection
- `fallback_to_kompress`: Use Kompress if tree-sitter unavailable
- `enable_ccr`: Store original in compression cache
- `ccr_ttl`: Cache TTL in seconds

#### Safety Protections (Applied at Transform Level)

**File**: `/tmp/headroom/docs/LIMITATIONS.md` Lines 21-35
- **Word count gate**: Content under 50 words skipped
- **Recent code protection**: Code in last 4 messages never compressed
- **Analysis intent protection**: If recent user message contains keywords ("analyze", "review", "explain", "fix", "debug", "optimize", "error", "bug"), ALL code protected

**Reason**: Code almost always fetched because user wants to work with it. Compression removes exactly what they need.

---

### 3. Kompress (NLP Text Compression)

**File**: `/tmp/headroom/headroom/transforms/kompress_compressor.py` (Full file: 507 lines)

#### Model Architecture

**Base**: ModernBERT (answerdotai/ModernBERT-base, 768 hidden size)

**Dual-head architecture**:

##### Head 1: Token Keep/Discard
- Linear layer: hidden_size → 2 classes
- **Decision**: Binary classification (keep vs discard)
- **Softmax**: Argmax(class_1_logits > class_0_logits)

##### Head 2: Span Importance (1D CNN)
```
Input: [seq_len, 768]
  → Conv1d(768, 256, kernel=5, padding=2)
  → GELU activation
  → Conv1d(256, 1, kernel=3, padding=1)
  → Sigmoid()
Output: [seq_len] span importance scores
```

#### Inference Process

**Lines**: 388-507
1. **Tokenization**: Split input into words
2. **Batching**: Chunk at 512 tokens (model max_length)
3. **Scoring**: Per-token keep probability + span importance
4. **Decision**:
   - Default: Keep if token_head says keep OR (borderline AND span_boost)
   - With target_ratio: Rank by score, keep top N%
5. **Reconstruction**: Reassemble kept words in original order

#### Model Loading

**Dual strategy**:

**Option 1: ONNX (default, lightweight)**
- File: `/tmp/headroom/headroom/transforms/kompress_compressor.py` Lines 166-190
- **Size**: 50MB ONNX INT8 quantized model
- **Download**: huggingface_hub.hf_hub_download(chopratejas/kompress-base, onnx/kompress-int8.onnx)
- **Runtime**: onnxruntime (no torch dependency)
- **Speed**: Inference without PyTorch overhead

**Option 2: PyTorch (full, more flexible)**
- Lines 193-234
- **Size**: Model.safetensors (~400MB)
- **Runtime**: torch + transformers
- **Device**: Auto-detect (CUDA > MPS > CPU)
- **Fallback**: If ONNX fails, tries PyTorch

#### Configuration

**Class**: `KompressConfig`
- `device`: "auto", "cuda", "cpu", "mps"
- `enable_ccr`: Store original for retrieval

#### Handling

- Chunks longer than 512 tokens per model limit
- Word-level deduplication (tracks kept word indices)
- Protected tags restored after compression
- Graceful fallback if model unavailable

---

### 4. ContentRouter (Compression Orchestrator)

**File**: `/tmp/headroom/headroom/transforms/content_router.py` (Lines 593-2042)

#### Core Logic

**Pipeline**:
1. Detect content type from content itself
2. Determine compression strategy
3. Route to appropriate compressor
4. Record to TOIN for cross-user learning

#### Content Detection

**Function**: `_detect_content()` (Lines 68-88)
- **Uses**: Magika (magic file detection) if available
- **Fallback**: Heuristic patterns (JSON check, code fence detection, etc.)
- **Returns**: `DetectionResult(content_type: ContentType, confidence: float)`

#### Strategy Determination

**Function**: `_determine_strategy()` (Lines 769-784)
1. Check for mixed content (code + text + JSON)
2. Detect pure content type
3. Map to strategy

#### Content Type → Compression Strategy Mapping

```python
ContentType.SOURCE_CODE       → CompressionStrategy.CODE_AWARE
ContentType.JSON_ARRAY        → CompressionStrategy.SMART_CRUSHER
ContentType.SEARCH_RESULTS    → CompressionStrategy.SEARCH
ContentType.BUILD_OUTPUT      → CompressionStrategy.LOG
ContentType.GIT_DIFF          → CompressionStrategy.DIFF
ContentType.HTML              → CompressionStrategy.HTML
ContentType.PLAIN_TEXT        → CompressionStrategy.TEXT
```

#### Strategy Application

**Function**: `_apply_strategy_to_content()` (Lines 924-1036)

Each strategy routes to appropriate compressor:
- **CODE_AWARE**: CodeCompressor (with fallback to Kompress)
- **SMART_CRUSHER**: SmartCrusher instance
- **SEARCH**: SearchCompressor (not detailed in provided code)
- **LOG**: LogCompressor (not detailed)
- **DIFF**: DiffCompressor (not detailed)
- **HTML**: HTMLExtractor
- **KOMPRESS**: KompressCompressor directly
- **TEXT**: ML-based compression (Kompress)

#### TOIN Recording

**Function**: `_record_to_toin()` (Lines 654-726)
- Records successful compressions to Tool Output Intelligence Network
- Tracks: strategy, original tokens, compressed tokens, language, context
- Enables cross-user learned patterns

#### Mixed Content Handling

**Function**: `_compress_mixed()` (Lines 816-882)
1. Split content into sections (code fences, JSON blocks, text)
2. Detect each section's type
3. Compress section with appropriate strategy
4. Preserve code fence markers
5. Reassemble with newlines

---

## PART 2: RTK (Rust CLI Filtering)

### Architecture Overview

**File**: `/tmp/rtk/ARCHITECTURE.md`

RTK is a command proxy with 42 command modules + 22 infrastructure modules, achieving 60-99% token savings through intelligent output filtering.

### 12 Filtering Strategies

#### 1. Stats Extraction
- **Example**: git log → "5 commits, +142/-89"
- **Reduction**: 90-99%
- **Used by**: git commands, pnpm list

#### 2. Error Only
- **Method**: Extract stderr only, drop stdout
- **Reduction**: 60-80%
- **Used by**: Test runners, build tools

#### 3. Grouping by Pattern
- **Method**: Group errors by rule/code, count occurrences
- **Example**: "no-unused-vars: 23 errors"
- **Reduction**: 80-90%
- **Used by**: Lint (ESLint), TypeScript, grep

#### 4. Deduplication
- **Method**: Unique items + occurrence count
- **Example**: "Log line ... (×47)"
- **Reduction**: 70-85%
- **Used by**: Log aggregation

#### 5. Structure Only
- **Method**: Extract JSON keys + types, strip values
- **Example**: `{user: {...}, posts: [...]}`
- **Reduction**: 80-95%
- **Used by**: json_cmd (schema extraction)

#### 6. Code Filtering
- **File**: `/tmp/rtk/src/core/filter.rs`
- **Levels**:
  - **None**: Keep all (0%)
  - **Minimal**: Strip comments only (20-40%)
  - **Aggressive**: Strip comments + bodies (60-90%)
- **Languages**: Rust, Python, JavaScript, TypeScript, Go, C, C++, Java, Ruby, Shell
- **Detection**: File extension-based with fallback

#### 7. Failure Focus
- **Method**: Keep failures only, hide passing items
- **Example**: "2 failed, 18 passed" → Just failures listed
- **Reduction**: 94-99%
- **Used by**: Test runners (vitest, playwright, pytest)

#### 8. Tree Compression
- **Method**: File list → directory tree with aggregates
- **Example**: "src/ (12 files)" vs listing all
- **Reduction**: 50-70%
- **Used by**: ls command

#### 9. Progress Filtering
- **Method**: Strip ANSI escape sequences, keep final result
- **Reduction**: 85-95%
- **Used by**: wget, pnpm install

#### 10. JSON/Text Dual Mode
- **Method**: JSON when available (structured), text fallback
- **Used by**: ruff, pip (intelligently chooses format)
- **Reduction**: 80%+

#### 11. State Machine Parsing
- **Method**: Track test state (IDLE → START → PASSED/FAILED → SUMMARY)
- **Extraction**: Test names, outcomes, failures only
- **Reduction**: 90%+
- **Used by**: pytest, vitest

#### 12. NDJSON Streaming
- **Method**: Parse line-by-line JSON events, aggregate
- **Example**: "2 fail (pkg1, pkg2)" from full output
- **Reduction**: 90%+
- **Used by**: go test (NDJSON events)

### Code Filtering Implementation

**File**: `/tmp/rtk/src/core/filter.rs` (Lines 1-150)

#### Language Detection

```rust
pub fn from_extension(ext: &str) -> Language {
    match ext.to_lowercase().as_str() {
        "rs" => Language::Rust,
        "py" | "pyw" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "ts" | "tsx" => Language::TypeScript,
        "go" => Language::Go,
        "c" | "h" => Language::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => Language::Cpp,
        "java" => Language::Java,
        "rb" => Language::Ruby,
        "sh" | "bash" | "zsh" => Language::Shell,
        // Data formats (no comment stripping)
        "json" | "yaml" | "toml" | "xml" | "csv" => Language::Data,
        ...
    }
}
```

#### Comment Patterns by Language

| Language | Line | Block Start | Block End | Doc Line | Doc Block |
|----------|------|-------------|-----------|----------|-----------|
| Rust | `//` | `/*` | `*/` | `///` | `/**` |
| Python | `#` | `"""` | `"""` | None | `"""` |
| JS/TS/Go/C/C++/Java | `//` | `/*` | `*/` | None | `/**` |
| Ruby | `#` | `=begin` | `=end` | None | None |
| Shell | `#` | None | None | None | None |
| Data | None | None | None | None | None |

### Command Lifecycle (6 Phases)

**File**: `/tmp/rtk/ARCHITECTURE.md` Lines 57-141

```
Phase 1: PARSE       → Extract command + args via Clap
Phase 2: ROUTE       → main.rs matches Commands enum
Phase 3: EXECUTE     → std::process::Command runs native tool
Phase 4: FILTER      → Strategy-specific output filtering
Phase 5: PRINT       → Colored output to stdout
Phase 6: TRACK       → SQLite records: original tokens → compressed tokens
```

### Token Tracking

**File**: `/tmp/rtk/src/core/tracking.rs`
- Records per command: input tokens, output tokens, savings %
- Database: `~/.local/share/rtk/history.db`
- Estimation: Token count = character_count / 4 (simple approximation)

### Module Organization

**42 Command Modules** in `src/cmds/`:
- **GIT** (7 ops): status, diff, log, add, commit, push, branch
- **JS/TS** (8 tools): lint, tsc, next, prettier, playwright, prisma, vitest, pnpm
- **Python** (3 tools): ruff, pytest, pip
- **Go** (2 tools): go test/build/vet, golangci-lint
- **Ruby** (3 tools): rake, rspec, rubocop
- **DOTNET** (2 tools): build, test
- **Cloud** (5 tools): aws, docker, kubectl, curl, wget
- **System** (8 tools): ls, tree, read, grep, find, json, log, env
- **Rust** (5 tools): cargo test/build/clippy, err

---

## PART 3: PROXY PIPELINE & CONSERVATION RULES

### Headroom Compression Pipeline Order

**File**: `/tmp/headroom/headroom/compress.py` Lines 94-181

```python
def _get_pipeline() -> Any:
    return TransformPipeline([
        CacheAligner(),        # 1. Align with previous cache state
        ContentRouter(),       # 2. Route content to appropriate compressor
        IntelligentContext()   # 3. Final context window management
    ])
```

**Execution order**:
1. **CacheAligner**: Check compression history, align recommendations
2. **ContentRouter**: Auto-detect content, apply best compressor (SmartCrusher/CodeCompressor/Kompress)
3. **IntelligentContext**: Final token budget enforcement

### What NEVER Gets Compressed (Conservation Rules)

**File**: `/tmp/headroom/docs/LIMITATIONS.md`

#### JSON Constraints
Lines 37-53:
- **Below 5 items**: Skipped (too small)
- **Below 200 tokens**: Skipped (too small)
- **Bool-only arrays**: Not useful, skipped
- **Objects without arrays**: No compression benefit, skipped
- **Malformed JSON**: Silently passes through (fail-safe)
- **Nesting depth > 5**: Inner arrays not examined

#### Code Constraints
Lines 21-35:
- **Under 50 words**: Word count gate, skipped
- **Last 4 messages**: Recent code protection, never compressed
- **User analysis context**: If recent message has keywords ("analyze", "review", "explain", "fix", "debug", "optimize", "error", "bug"), ALL code protected

#### Text Constraints
Lines 76-84:
- **Under 100 tokens**: Skipped (too small)
- **First call**: 10-30s model load latency (cached after)

#### Error Handling
Lines 85-96:
- **Invalid JSON**: Passthrough (no error raised)
- **AST parse failure**: Fallback to original or Kompress
- **Compression enlarges output**: Original returned
- **Missing dependencies**: Passthrough with warning

#### Adaptive Safety
All compression includes **automatic fallback**:
- If compression ratio < threshold (varies by strategy)
- If syntax invalid (code compression)
- If output larger than input

---

## Summary Table: When Each Compressor Fires

| Input Type | Detector | Strategy | Compressor | Reduction |
|-----------|----------|----------|-----------|-----------|
| JSON array of dicts | Magika/regex | SMART_CRUSHER | SmartCrusher | 80-99% |
| JSON array of strings | Magika/regex | SMART_CRUSHER | SmartCrusher | 60-90% |
| Source code (Python/Rust/JS) | Magic bytes + language detection | CODE_AWARE | CodeCompressor | 40-80% |
| Plain text (log, article) | Content heuristics | TEXT/KOMPRESS | Kompress | 43-46% |
| Mixed (code + text + JSON) | Pattern detection | MIXED | Each section individually routed | 60-90% |
| Search results JSON | Domain detection | SEARCH | SearchCompressor | 70-90% |
| Build logs (stderr) | Tool detection | LOG | LogCompressor | 80-95% |
| Git diff output | Diff marker detection | DIFF | DiffCompressor | 80-95% |
| HTML content | Tag detection | HTML | HTMLExtractor | 60-85% |

---

## File Path Reference

### Headroom
- `/tmp/headroom/headroom/transforms/smart_crusher.py` — SmartCrusher (JSON)
- `/tmp/headroom/headroom/transforms/code_compressor.py` — CodeCompressor (AST)
- `/tmp/headroom/headroom/transforms/kompress_compressor.py` — Kompress (NLP)
- `/tmp/headroom/headroom/transforms/content_router.py` — ContentRouter (orchestrator)
- `/tmp/headroom/headroom/transforms/anchor_selector.py` — AnchorSelector (dynamic anchors)
- `/tmp/headroom/headroom/config.py` — Configuration classes
- `/tmp/headroom/docs/LIMITATIONS.md` — Conservation rules
- `/tmp/headroom/docs/transforms.md` — Transform documentation

### RTK
- `/tmp/rtk/src/main.rs` — CLI entry, command routing
- `/tmp/rtk/src/core/filter.rs` — Code filtering by language
- `/tmp/rtk/src/cmds/` — 42 command-specific modules
- `/tmp/rtk/ARCHITECTURE.md` — Full architecture details
- `/tmp/rtk/src/core/tracking.rs` — Token tracking
