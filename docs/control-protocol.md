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

## MCP server (`sp mcp`)

`sp mcp [--open book.xlsx]` speaks the [Model Context Protocol](https://modelcontextprotocol.io)
over stdio: JSON-RPC 2.0, one message per line, protocol versions `2025-06-18`, `2025-03-26`
and `2024-11-05` (the client's choice when supported, else the newest). Methods:
`initialize`, `ping`, `tools/list`, `tools/call` (plus empty `resources/list` and
`prompts/list`); notifications get no reply; batches (arrays) are accepted. Code:
`apps/sp/src/mcp.rs`. Logs go to stderr; stdout carries only protocol messages.

| Tool | Arguments | Result |
|---|---|---|
| `list_commands` | `group`? (id prefix like `home.font`, or ribbon tab), `query`? (text), `limit`? | `{total, commands: [{id, label, location, shortcut, params, enabled}]}` |
| `run_command` | `id`, `params`? (object) | `{id, result}`: the command's outcome |
| `get_cells` | `range` (`A1:C9`, `Sheet2!B3`, a name) | `{sheet, range, rows: [[{value, formula?, input, display}]]}` |
| `set_cells` | `range` (top-left cell or range), `values` (2-D array of string/number/bool/null) | `{written, range}`; strings parse like typing (`=` formula, `'` text), null clears; one undo step |
| `open_workbook` | `path` | sheet list |
| `save_workbook` | `path`? (save as; omitted: save) | `{saved}` |
| `list_sheets` | | sheets with used ranges, active sheet, selection, path, dirty, undo/redo |
| `screenshot` | `width`?, `height`? (200-4096) | MCP `image` content, PNG of the real shell |
| `undo` / `redo` | | label of the step |

Tool failures (bad range, unknown command id, refused edit, failed save) come back as
results with `isError: true` and a text message; an unknown tool name or malformed request
is a JSON-RPC error. Limits: 16 MiB per message, 100,000 cells per `get_cells`/`set_cells`
call, 1,000 messages per batch. No tool touches the network.

Client configuration (Claude Desktop / Claude Code `.mcp.json` and most MCP clients):

```json
{
  "mcpServers": {
    "sheetpro": {
      "command": "/path/to/sp",
      "args": ["mcp"]
    }
  }
}
```

From a checkout: `"command": "cargo", "args": ["run", "-q", "-p", "sp", "--", "mcp"]`
(or `claude mcp add sheetpro -- /path/to/sp mcp`). The `apps/sp/tests/agent_tasks.rs`
suite drives the binary this way through ten end-to-end workflows.

## Macros and spelling

- `view.macros`: `{"action": "record_start", "name": "Fmt"}` … any commands …
  `{"action": "record_stop"}`; `run`, `list`, `get` (returns `jsonl`), `set` (`name` +
  `jsonl`, one `{"id": …, "params": {…}}` per line, `#` comments allowed), `delete`,
  `record_cancel`. Stored in the workbook as `customXml/sheetproMacros.xml`; running one is
  a single undo step and a failing step rolls the run back. Macros cannot run macros.
- `review.spelling`: `check` (default; `range`, else the selection, else the whole sheet) →
  `{misspellings: [{cell, word, offset, suggestions}]}`; `suggest`, `ignore` (`cell`,
  `word`), `ignore_all`, `add` (user dictionary in `settings.toml`), `change` (`cell`,
  `word`, `to`), `change_all` (`word`, `to`, `range`?).
