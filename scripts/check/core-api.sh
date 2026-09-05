#!/usr/bin/env bash
# The negotiated tilth-core API, from outside the crate: the acceptance test
# weed asked for compiles against every requested function and type and
# passes, and rustdoc for the crate builds with missing docs denied, so what
# the crate documents is complete.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo test -p tilth-core --test api
RUSTDOCFLAGS="-D missing_docs -D rustdoc::broken_intra_doc_links" cargo doc -p tilth-core --no-deps --quiet
echo "tilth-core API: acceptance test green, docs complete"
