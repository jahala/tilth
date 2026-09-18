#!/usr/bin/env bash
# Output parity: the tilth binary built from this workspace produces the same
# bytes as the v0.10.1 release binary, over a fixed matrix of CLI commands and
# MCP sessions on the benchmark fixture repos (ripgrep, fastapi, gin, express
# at the commits benchmark/config.py pins).
#
# The reference runs REF_RUNS times (default 3). A command whose reference
# output is not stable across its own runs is reported as such, and compared
# modulo order instead: per line, commas and JSON `\n`/`\t` escapes become
# spaces, whitespace tokens are sorted; then lines sorted. Stable commands must match byte for byte (stdout, stderr,
# exit code). PARITY_SKIP_RUNS=1 re-compares an existing PARITY_WORK dir.
#
#   TILTH_REF    reference binary; default: build v0.10.1 from the tag
#   TILTH_NEW    candidate binary; default: cargo build --release here
#   REPOS_DIR    fixture repos (default /tmp/tilth_bench/repos, as config.py)
#   PARITY_WORK  scratch dir (default: mktemp under /tmp, kept for inspection)
#
# Prints a markdown report on stdout. Exit 1 on any mismatch.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD
REPOS_DIR="${REPOS_DIR:-/tmp/tilth_bench/repos}"
REF_RUNS="${REF_RUNS:-3}"
WORK="${PARITY_WORK:-$(mktemp -d /tmp/tilth-parity.XXXXXX)}"
mkdir -p "$WORK"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -c1-16; else shasum -a 256 | cut -c1-16; fi; }
say() { echo "$*" >&2; }

# ── fixtures ────────────────────────────────────────────────────────────────
missing=0
for r in ripgrep fastapi gin express; do [[ -d $REPOS_DIR/$r ]] || missing=1; done
if (( missing )); then say "fixture repos missing under $REPOS_DIR — running benchmark/fixtures/setup_repos.py"; python3 benchmark/fixtures/setup_repos.py >&2; fi
for r in ripgrep fastapi gin express; do
  if [[ -n $(git -C "$REPOS_DIR/$r" status --porcelain) ]]; then say "✗ fixture $r is dirty; refusing to measure on a modified tree"; exit 2; fi
done

# ── binaries ────────────────────────────────────────────────────────────────
if [[ -z ${TILTH_REF:-} && -n ${PARITY_SKIP_RUNS:-} ]]; then TILTH_REF="$WORK/ref-src/target/release/tilth"; fi
if [[ -z ${TILTH_NEW:-} && -n ${PARITY_SKIP_RUNS:-} ]]; then TILTH_NEW="$ROOT/target/release/tilth"; fi
if [[ -z ${TILTH_REF:-} ]]; then
  refdir="$WORK/ref-src"
  if [[ ! -x $refdir/target/release/tilth ]]; then
    say "building the v0.10.1 reference from the tag into $refdir"
    mkdir -p "$refdir"; git archive v0.10.1 | tar -x -C "$refdir"
    (cd "$refdir" && cargo build --release --locked --quiet)
  fi
  TILTH_REF="$refdir/target/release/tilth"
fi
if [[ -z ${TILTH_NEW:-} ]]; then
  say "building the candidate"
  cargo build --release --quiet
  TILTH_NEW="$ROOT/target/release/tilth"
fi
TILTH_REF=$(cd "$(dirname "$TILTH_REF")" && pwd -P)/$(basename "$TILTH_REF")
TILTH_NEW=$(cd "$(dirname "$TILTH_NEW")" && pwd -P)/$(basename "$TILTH_NEW")

# ── one run of the matrix ───────────────────────────────────────────────────
run_cli() {
  local name=$1; shift
  local out="$OUT/$name"
  set +e
  ( cd "$REPO" && TILTH_THREADS=1 PAGER=cat "$BIN" "$@" >"$out.stdout" 2>"$out.stderr" ); echo $? >"$out.exit"
  set -e
}
run_mcp() {
  local name=$1 transcript=$2; shift 2
  local out="$OUT/$name"
  set +e
  ( cd "$REPO" && printf '%s\n' "$transcript" | TILTH_THREADS=1 "$BIN" --mcp "$@" >"$out.stdout" 2>"$out.stderr" ); echo $? >"$out.exit"
  set -e
}
transcript_readonly() {
  jq -nc --arg root "$1" --arg big "$2" --arg small "$3" --arg sym "$4" --arg grok "$5" --arg ext "$6" '
    {jsonrpc:"2.0",id:1,method:"initialize",params:{protocolVersion:"2025-06-18",capabilities:{},clientInfo:{name:"parity",version:"0"}}},
    {jsonrpc:"2.0",id:2,method:"tools/list",params:{}},
    {jsonrpc:"2.0",id:3,method:"tools/call",params:{name:"tilth_search",arguments:{query:$sym,root:$root}}},
    {jsonrpc:"2.0",id:4,method:"tools/call",params:{name:"tilth_search",arguments:{query:$sym,root:$root,expand:3}}},
    {jsonrpc:"2.0",id:5,method:"tools/call",params:{name:"tilth_search",arguments:{query:$grok,kind:"callers",root:$root}}},
    {jsonrpc:"2.0",id:6,method:"tools/call",params:{name:"tilth_search",arguments:{query:"TODO",kind:"content",root:$root}}},
    {jsonrpc:"2.0",id:7,method:"tools/call",params:{name:"tilth_read",arguments:{path:$big,root:$root}}},
    {jsonrpc:"2.0",id:8,method:"tools/call",params:{name:"tilth_read",arguments:{path:$small,section:"1-40",root:$root}}},
    {jsonrpc:"2.0",id:9,method:"tools/call",params:{name:"tilth_read",arguments:{paths:[$small,$big],root:$root}}},
    {jsonrpc:"2.0",id:10,method:"tools/call",params:{name:"tilth_deps",arguments:{path:$big,root:$root}}},
    {jsonrpc:"2.0",id:11,method:"tools/call",params:{name:"tilth_grok",arguments:{target:$grok,root:$root}}},
    {jsonrpc:"2.0",id:12,method:"tools/call",params:{name:"tilth_list",arguments:{patterns:[$ext],root:$root}}},
    {jsonrpc:"2.0",id:13,method:"tools/call",params:{name:"tilth_diff",arguments:{source:"HEAD~1",root:$root}}},
    {jsonrpc:"2.0",id:14,method:"tools/call",params:{name:"tilth_search",arguments:{query:$sym,root:$root,expand:3}}},
    {jsonrpc:"2.0",id:15,method:"tools/call",params:{name:"tilth_savings",arguments:{}}},
    {jsonrpc:"2.0",id:16,method:"tools/call",params:{name:"tilth_read",arguments:{path:"no/such/file.txt",root:$root}}},
    {jsonrpc:"2.0",id:17,method:"tools/call",params:{name:"no_such_tool",arguments:{}}},
    {jsonrpc:"2.0",id:18,method:"ping",params:{}}'
}
transcript_edit() {
  jq -nc --arg root "$1" --arg small "$2" '
    {jsonrpc:"2.0",id:1,method:"initialize",params:{protocolVersion:"2025-06-18",capabilities:{},clientInfo:{name:"parity",version:"0"}}},
    {jsonrpc:"2.0",id:2,method:"tools/list",params:{}},
    {jsonrpc:"2.0",id:3,method:"tools/call",params:{name:"tilth_read",arguments:{path:$small,section:"1-30",root:$root}}},
    {jsonrpc:"2.0",id:4,method:"tools/call",params:{name:"tilth_write",arguments:{files:[{path:$small,edits:[{start:"1:ZZZZ",content:"// parity probe"}]}],root:$root}}},
    {jsonrpc:"2.0",id:5,method:"tools/call",params:{name:"tilth_savings",arguments:{}}}'
}
matrix() {
  local big small sym sym2 callers grok ext sub regex
  case $REPO_NAME in
    ripgrep) big=crates/core/flags/defs.rs; small=crates/core/main.rs; sym=Searcher; sym2=Sink; callers=SearcherBuilder; grok=Searcher; ext='*.rs'; sub=crates/core; regex='/fn \w+_test/';;
    fastapi) big=fastapi/routing.py; small=fastapi/params.py; sym=Depends; sym2=APIRouter; callers=solve_dependencies; grok=APIRoute; ext='*.py'; sub=fastapi/dependencies; regex='/def \w+_dependant/';;
    gin)     big=context.go; small=gin.go; sym=Context; sym2=Engine; callers=Abort; grok=Next; ext='*.go'; sub=binding; regex='/func \(c \*Context\) \w+/';;
    express) big=lib/application.js; small=lib/response.js; sym=createApplication; sym2=send; callers=handle; grok=createApplication; ext='*.js'; sub=lib; regex='/function \w+\(req, res/';;
  esac
  run_cli read-outline "$big"
  run_cli read-small "$small"
  run_cli read-section "$big" --section 1-60
  run_cli read-full "$small" --full
  run_cli read-json "$small" --json
  run_cli read-budget "$big" --budget 400
  run_cli read-markdown README.md
  run_cli search-symbol "$sym" --scope .
  run_cli search-expand "$sym" --scope . --expand
  run_cli search-expand5 "$sym2" --scope . --expand=5
  run_cli search-full "$sym2" --scope . --full
  run_cli search-glob "$sym" --scope . --glob "$ext"
  run_cli search-multi "$sym,$sym2" --scope .
  run_cli search-content TODO --scope .
  run_cli search-regex "$regex" --scope .
  run_cli search-json "$sym" --scope . --json
  run_cli callers "$callers" --scope . --callers
  run_cli callers-expand "$callers" --scope . --callers --expand
  run_cli deps "$big" --deps
  run_cli glob "$ext" --scope "$sub"
  run_cli map --map --scope "$sub"
  run_cli grok grok "$grok"
  run_cli grok-full grok "$grok" --full
  run_cli diff diff HEAD~1
  run_cli diff-log diff --log HEAD~3..HEAD
  run_cli overview overview
  run_cli err-missing no_such_file.xyz
  run_cli err-multi "a,b,c,d,e,f,g" --scope .
  local root; root=$(cd "$REPO" && pwd -P)
  local t_ro t_ed
  t_ro=$(transcript_readonly "$root" "$big" "$small" "$sym" "$grok" "$ext")
  t_ed=$(transcript_edit "$root" "$small")
  run_mcp mcp-readonly "$t_ro"
  TILTH_NO_OVERVIEW=1 run_mcp mcp-readonly-nofingerprint "$t_ro"
  run_mcp mcp-edit "$t_ed" --edit
  TILTH_NO_OVERVIEW=1 run_mcp mcp-edit-nofingerprint "$t_ed" --edit
}

labels=()
for i in $(seq 1 "$REF_RUNS"); do labels+=("ref$i"); done
labels+=(new1 new2)
[[ -n ${PARITY_SKIP_RUNS:-} ]] && labels=()
for label in ${labels[@]+"${labels[@]}"}; do
  case $label in ref*) BIN=$TILTH_REF;; *) BIN=$TILTH_NEW;; esac
  for REPO_NAME in ripgrep fastapi gin express; do
    REPO="$REPOS_DIR/$REPO_NAME"; OUT="$WORK/$label/$REPO_NAME"; mkdir -p "$OUT"
    say "  $label · $REPO_NAME"
    matrix
    if [[ -n $(git -C "$REPO" status --porcelain) ]]; then say "✗ the matrix modified fixture $REPO_NAME; aborting"; git -C "$REPO" status --porcelain >&2; exit 2; fi
  done
done

# ── compare ─────────────────────────────────────────────────────────────────
digest() { cat "$WORK/$1/$2/$3.stdout" "$WORK/$1/$2/$3.stderr" "$WORK/$1/$2/$3.exit" | sha; }
normalized() {
  cat "$WORK/$1/$2/$3.stdout" "$WORK/$1/$2/$3.stderr" "$WORK/$1/$2/$3.exit" \
    | python3 -c 'import sys; print("\n".join(sorted(" ".join(sorted(l.replace(",", " ").replace("\\n", " ").replace("\\t", " ").split())) for l in sys.stdin.read().split("\n"))))' | sha
}
total=0; identical=0; modulo=0; mismatches=0
rows=()
for REPO_NAME in ripgrep fastapi gin express; do
  for f in "$WORK/ref1/$REPO_NAME"/*.exit; do
    case_name=$(basename "$f" .exit); total=$((total+1))
    ref_digests=(); for i in $(seq 1 "$REF_RUNS"); do ref_digests+=("$(digest "ref$i" "$REPO_NAME" "$case_name")"); done
    stable=yes; for d in "${ref_digests[@]}"; do [[ $d == "${ref_digests[0]}" ]] || stable=no; done
    n1=$(digest new1 "$REPO_NAME" "$case_name"); n2=$(digest new2 "$REPO_NAME" "$case_name")
    bytes=$(wc -c < "$WORK/ref1/$REPO_NAME/$case_name.stdout" | tr -d ' ')
    code=$(cat "$WORK/ref1/$REPO_NAME/$case_name.exit")
    if [[ $stable == yes ]]; then
      if [[ $n1 == "${ref_digests[0]}" && $n2 == "${ref_digests[0]}" ]]; then verdict="identical"; identical=$((identical+1)); else verdict="**MISMATCH**"; mismatches=$((mismatches+1)); fi
    else
      r0=$(normalized ref1 "$REPO_NAME" "$case_name"); ok=yes
      for i in $(seq 2 "$REF_RUNS"); do [[ $(normalized "ref$i" "$REPO_NAME" "$case_name") == "$r0" ]] || ok=no; done
      [[ $(normalized new1 "$REPO_NAME" "$case_name") == "$r0" && $(normalized new2 "$REPO_NAME" "$case_name") == "$r0" ]] || ok=no
      if [[ $ok == yes ]]; then verdict="identical modulo order"; modulo=$((modulo+1)); else verdict="**MISMATCH beyond order**"; mismatches=$((mismatches+1)); fi
    fi
    rows+=("| $REPO_NAME | \`$case_name\` | $bytes | $code | $stable | $verdict |")
  done
done

echo "# Output parity — workspace tilth vs v0.10.1"
echo
echo "- reference: \`$("$TILTH_REF" --version)\` built from tag v0.10.1 ($(git rev-parse --short v0.10.1)), run $REF_RUNS times"
echo "- candidate: \`$("$TILTH_NEW" --version)\` built from $(git rev-parse --short HEAD) on branch $(git branch --show-current), run twice"
echo "- fixtures: $(for r in ripgrep fastapi gin express; do printf '%s@%s ' "$r" "$(git -C "$REPOS_DIR/$r" rev-parse --short HEAD)"; done)"
echo "- environment: TILTH_THREADS=1, PAGER=cat, stdout piped; MCP sessions over stdio with and without the project fingerprint"
echo "- date: $(date -u +%Y-%m-%d)"
echo
echo "| cases | identical | identical modulo order | mismatches |"
echo "|---|---|---|---|"
echo "| $total | $identical | $modulo | $mismatches |"
echo
echo "\"stable\" means the reference's own $REF_RUNS runs agreed byte for byte. Where they did not, the outputs are compared with each line's tokens sorted and lines sorted; every such case is named so the nondeterminism is visible, not hidden."
echo
echo "| repo | case | ref bytes | exit | ref stable | verdict |"
echo "|---|---|---|---|---|---|"
printf '%s\n' "${rows[@]}"
echo
echo "scratch: $WORK"
(( mismatches == 0 ))
