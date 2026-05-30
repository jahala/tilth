# tilth wiki — index

This wiki is what an LLM working in the `tilth` repo writes to and reads from
when it wants to remember things across sessions. It lives at
`.hallouminate/wiki/` and is indexed as the `repo:tilth:wiki` corpus, separate
from the source-code corpus (`repo:tilth:corpus`) and the per-session reports
under `.cheese/`.

## Topics

_(none yet — add pages below as cross-session knowledge accrues)_

## How to use this index

`index.md` is a table of contents, not a topic. Add new pages to the list
above (alphabetical), keeping a one-line gloss per entry. Anything substantive
belongs in a topic file — one topic per file.

If you read this index and don't see the topic you need, run `list_files`
against the `repo:tilth:wiki` corpus first — the index may be out of date
relative to the directory.
