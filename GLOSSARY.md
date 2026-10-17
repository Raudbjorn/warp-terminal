# Conn

Conn is a re-entry layer for Claude Code sessions running in terminal panes: it tells a person
returning to a pane what they asked for, what the agent did, and where it stands now.

## Language

### Where agents run

**Pane**:
A terminal pane in a Warp window, identified across restarts by its UUID. A pane has at most one
current session.
_Avoid_: terminal session, tab

**Session**:
One Claude Code conversation, identified by its session id and backed by one transcript. A pane
can host several sessions over time.
_Avoid_: terminal session, agent run, conversation

**Project**:
The git repository a session runs in, named after its main worktree so that worktrees of one
repository are one project. Outside git, the session's working directory.
_Avoid_: repo, workspace, folder

**Transcript**:
The JSONL file Claude Code writes for a session as it works. It is the record Conn reads.

### What Conn tells

**Story**:
A session's turns, read from its transcript.
_Avoid_: log, history, timeline

**Turn**:
One typed prompt and everything the agent did until it stopped.
_Avoid_: chapter, exchange, prompt

**Step**:
One tool call inside a turn, in the agent's own narration.
_Avoid_: action, tool use, entry

**Answer**:
The closing message of a turn.
_Avoid_: outcome, response, reply

**Plan**:
The plan file a session presents for approval in plan mode.

### Where a session stands

**Status**:
Where a session stands, one of Working, Needs you, Done, Failed or Ended.
_Avoid_: state

**Needs you**:
The status of a session waiting on a permission, a question or plan approval.
_Avoid_: blocked, waiting

**Done**:
The status of a session that stopped, including one the user cancelled.
_Avoid_: success, cancelled, idle

**Ended**:
The status of a session whose agent process has exited.
_Avoid_: closed, finished

### Re-entry

**Seen**:
Activity is seen once its pane has been focused in the key window for two seconds.
_Avoid_: read, viewed

**Watermark**:
The time of the latest activity seen in a pane. Activity after it is unseen.
_Avoid_: read marker, cursor

**Unseen turns**:
Turns in a pane's current session whose answer arrived after the watermark.
_Avoid_: unread, new messages

**Overview**:
The list of every pane with a session, across all windows, grouped by project.
_Avoid_: dashboard, session list
