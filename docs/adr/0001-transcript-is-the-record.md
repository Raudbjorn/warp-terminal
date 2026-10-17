# The transcript is the record, Conn persists only an index

Conn does not store an event log. Claude Code's transcript already holds every turn, step and
answer in full, so Conn persists one small SQLite row per pane (current session, transcript path,
last status, watermark) and rebuilds the story from the transcript after a restart. Hook-only
entries for a turn in flight are lost on restart, which costs nothing because the agent process
dies with Warp.

## Considered Options

- Append each hook event to SQLite as it arrives (`fork/conn-plan.md` §4). Rejected: it
  duplicates the transcript with less detail, because the plugin truncates prompts and omits
  tool input.
