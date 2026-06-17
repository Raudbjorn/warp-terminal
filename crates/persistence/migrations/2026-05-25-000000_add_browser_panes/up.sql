-- oh-my-warp: persist embedded browser panes so they survive app restart.
-- The pane's current URL is the only state we restore (CDP sessions themselves
-- are ephemeral and recreated on launch).
CREATE TABLE browser_panes (
    id INTEGER PRIMARY KEY NOT NULL REFERENCES pane_nodes(id),
    kind TEXT NOT NULL DEFAULT 'browser',
    url TEXT NOT NULL
);
