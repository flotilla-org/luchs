# Source history

The import starts from `rjwittams/katzensteg` revision
`c981b7d482d57ddd0f16fae9b1c363b741a9b3a6`, available in `~/dev/katzensteg`.
`git-filter-repo` retained the helper, test pages and README, preserving authors,
commit dates and messages. The latest retained source change is
`16e6790` (Katzensteg PR #43). The Rust implementation follows the import merge.

| Katzensteg path | Luchs path |
| --- | --- |
| `tools/luchs/native/macos/LuchsWebviewCapture.swift` | `native/macos/LuchsWebviewCapture.swift` |
| `tools/luchs/testdata/` | `testdata/` |
| `tools/luchs/README.md` | `docs/history/README.md` |

The [source commit map](source-commit-map.tsv) is the filter's complete map.
All-zero destination IDs identify removed commits. The four retained commits
remain reachable through the import merge. The historical README describes the
old Zig/SDL prototype; the root README describes this Rust producer.

The import ran in a scratch clone outside the vessel checkout:

```sh
git filter-repo \
  --path tools/luchs/native/macos/LuchsWebviewCapture.swift \
  --path tools/luchs/testdata/ --path tools/luchs/README.md \
  --path-rename tools/luchs/README.md:docs/history/README.md \
  --path-rename tools/luchs/native/:native/ \
  --path-rename tools/luchs/testdata/:testdata/
```

No Zig core, SDL presenter or SDL-to-JSON forwarding was imported. Katzensteg's
source checkout was not changed.
