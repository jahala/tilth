# tilth — voice delta

Inherits the plotplot umbrella voice (calm · precise · literate · a little wit). One signature line and a terminology table, merged with the umbrella at read time.

**Signature phrase:** *give grep, tree-sitter, and cat a shared brain.*

## Terminology

| Use | Not | Why |
|---|---|---|
| outline | summary, skeleton, table of contents | What tilth returns for a large file: definitions with line ranges, not a paraphrase. |
| definition-first | ranked, relevant | Search puts definitions before usages by construction, not by a score. |
| callers | references (when call sites are meant) | A caller is a call site found in the syntax tree; a usage is any mention. |
| blast radius | impact, dependents | What breaks when an export changes: the files and symbols that call it. |
| structural diff | semantic diff, smart diff | Function-level change detection over a patch: added, removed, modified, signature changed. |
| cost per correct answer | tokens saved, efficiency | The metric: spend divided by right answers; copeca's yardstick. |
| section | range, snippet | A line range or heading read from one file. |
| expand | show, inline | Print the full source of a match under its outline line. |

Product name is lowercase always: `tilth`. Tool names are `tilth_search`, `tilth_read`, `tilth_list`, `tilth_deps`, `tilth_grok`, `tilth_diff`, `tilth_write`: never capitalised, never hyphenated. The library is `tilth-core`, hyphenated, lowercase.
