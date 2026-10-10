# Control protocol

One JSON object per line in, one JSON object per line out. Same handler (`sp_engine::control`)
for the CLI (`sp control`), the desktop app (`sheetpro --control-port 7878`, TCP on 127.0.0.1)
and screenshot scripts (`sp screenshot --script steps.jsonl`).

## Requests

| `op` | Fields | Result |
|---|---|---|
| `exec` | `id`, `params` (object, optional) | `{"ok":true,"result":…}` command outcome |
| `get` | `range` (A1, optional sheet prefix) | 2-D array of `{input, value, display}` |
| `set` | `cell`, `input` | shorthand for `exec cell.set` |
| `state` | | selection, active sheet, sheets, dirty, path, view state |
| `commands` | | list of registered command specs (id, label, location, shortcut, params) |
| `functions` | | list of supported worksheet function names |
| `screenshot` | `out` (path) | desktop app / `sp screenshot` only: writes PNG |
| `quit` | | ends the session |

Errors: `{"ok":false,"error":"<message>"}`; the session keeps running.

## Examples

```jsonl
{"op":"exec","id":"cell.set","params":{"cell":"A1","input":"10"}}
{"op":"exec","id":"cell.set","params":{"cell":"A2","input":"=A1*2"}}
{"op":"get","range":"A1:A2"}
{"op":"exec","id":"select.range","params":{"range":"A1:A2"}}
{"op":"exec","id":"home.font.bold"}
{"op":"exec","id":"file.save_as","params":{"path":"/tmp/out.xlsx"}}
```

## Common params

- `cell`, `range`: A1 text; `Sheet2!B3`, `'My Sheet'!A1:C9` allowed. Defaults to the
  active cell / selection when omitted.
- `path`: absolute file path for file commands.
- Commands document their params in `CommandSpec.params`; `{"op":"commands"}` lists them.
