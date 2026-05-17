# OSS-Fuzz artifacts

Files in `projects/tilth/` are ready to submit to [google/oss-fuzz][oss-fuzz]
to enable continuous fuzzing on Google's infrastructure. Submission is a
manual step — these artifacts are kept here so the in-tree fuzz harness
and the OSS-Fuzz config stay aligned.

## What's here

- `projects/tilth/project.yaml` — OSS-Fuzz metadata. Lists maintainer
  contact, sanitizers, and the fuzzing engine.
- `projects/tilth/Dockerfile` — clones tilth into the OSS-Fuzz build
  environment. Starts from `gcr.io/oss-fuzz-base/base-builder-rust`.
- `projects/tilth/build.sh` — runs `cargo +nightly fuzz build` and
  copies each target binary + corpus into `$OUT`.

## Submission workflow

1. Fork [google/oss-fuzz][oss-fuzz].
2. Copy this directory's contents into `projects/tilth/` in your fork.
3. Locally validate the build harness works against OSS-Fuzz's helper:
   ```bash
   python infra/helper.py build_image tilth
   python infra/helper.py build_fuzzers tilth
   python infra/helper.py check_build tilth
   ```
4. Open a PR to google/oss-fuzz. Expect 1-4 weeks for review.
5. Once accepted: weekly status emails go to the contact in
   `project.yaml`. Crashes are filed as private issues on the
   tilth repository via OSS-Fuzz's automated bug filing.

## Why not just submit immediately

OSS-Fuzz onboarding asks for two human maintainers if available, a
description, and confirmation that the project is suitable. We're solo —
mention that in the PR description; Google accepts solo-maintained
projects with a brief explanation.

## Keeping in sync

If `fuzz/fuzz_targets/*.rs` gains a new target, update:

1. `projects/tilth/build.sh` — add the target name to the `for target in …` loops.
2. The OSS-Fuzz fork PR (or a follow-up PR after initial acceptance).

[oss-fuzz]: https://github.com/google/oss-fuzz
