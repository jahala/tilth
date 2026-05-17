#!/bin/bash -eu
# OSS-Fuzz build script for tilth.
#
# Builds three fuzz targets (outline, strip, diff_parse) defined in
# fuzz/fuzz_targets/, then copies the resulting libfuzzer binaries to
# the location OSS-Fuzz expects ($OUT).
#
# To test locally with the OSS-Fuzz harness:
#     git clone https://github.com/google/oss-fuzz && cd oss-fuzz
#     python infra/helper.py build_image tilth
#     python infra/helper.py build_fuzzers tilth
#     python infra/helper.py check_build tilth

cd $SRC/tilth

# cargo-fuzz build wires up libfuzzer with the right sanitizer flags.
cargo +nightly fuzz build -O

# Copy each target binary into $OUT (the OSS-Fuzz output dir).
for target in outline strip diff_parse; do
    cp fuzz/target/x86_64-unknown-linux-gnu/release/${target} \
       $OUT/${target}
done

# Seed corpus into OSS-Fuzz's expected location, if present.
for target in outline strip diff_parse; do
    if [ -d fuzz/corpus/${target} ]; then
        zip -j ${OUT}/${target}_seed_corpus.zip fuzz/corpus/${target}/*
    fi
done
