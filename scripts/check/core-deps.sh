#!/usr/bin/env bash
# tilth-core builds alone as a library, and its direct dependencies carry no
# CLI or MCP crate. The binary's faces stay in the binary.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p tilth-core --lib --quiet
direct=$(cargo tree -p tilth-core -e normal --depth 1 --prefix none | awk '{print $1}' | sort -u)
status=0
for bad in clap clap_complete terminal_size home percent-encoding libc crossbeam-channel strsim; do
  if grep -qx "$bad" <<<"$direct"; then echo "✗ $bad is a direct dependency of tilth-core" >&2; status=1; fi
done
(( status == 0 )) && echo "tilth-core builds alone; $(wc -l <<<"$direct" | tr -d ' ') direct dependencies, none of them a face"
exit $status
