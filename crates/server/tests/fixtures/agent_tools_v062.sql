-- Relevant tables and persisted allowlists from the 0.6.2 schema.
CREATE TABLE metro_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
INSERT INTO metro_meta VALUES
    ('schema_version', '3'), ('identity_namespace', 'dev'),
    ('routing_directedness_v2', '1');

CREATE TABLE spaces (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    runner_token_hash BLOB UNIQUE,
    runner_generation INTEGER NOT NULL DEFAULT 0,
    runner_drop_before INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    description TEXT NOT NULL DEFAULT ''
);
INSERT INTO spaces (id, name, owner_id, created_at)
VALUES ('s-0000000000000001', 'Upgraded Shell', 'alice', 0);

CREATE TABLE space_members (
    space_id TEXT NOT NULL REFERENCES spaces (id),
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    added_at INTEGER NOT NULL,
    PRIMARY KEY (space_id, user_id)
);
INSERT INTO space_members VALUES ('s-0000000000000001', 'alice', 'owner', 0);

CREATE TABLE space_rooms (
    space_id TEXT NOT NULL REFERENCES spaces (id),
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    parent_room_id TEXT,
    created_at INTEGER NOT NULL,
    anchor_event_id INTEGER,
    PRIMARY KEY (space_id, id),
    FOREIGN KEY (space_id, parent_room_id) REFERENCES space_rooms (space_id, id)
);
INSERT INTO space_rooms (space_id, id, name, created_at)
VALUES ('s-0000000000000001', 'lobby', 'Lobby', 0);

CREATE TABLE space_agents (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces (id),
    name TEXT NOT NULL,
    handle TEXT NOT NULL,
    description TEXT NOT NULL,
    instructions TEXT NOT NULL DEFAULT '',
    tools TEXT NOT NULL DEFAULT '[]',
    is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    color TEXT NOT NULL,
    avatar_png BLOB,
    avatar_version TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (space_id, handle)
);
CREATE INDEX space_agents_space ON space_agents (space_id, created_at);
CREATE UNIQUE INDEX space_agents_one_default ON space_agents (space_id) WHERE is_default = 1;
INSERT INTO space_agents (id, space_id, name, handle, description, tools,
                          is_default, color, created_at, updated_at)
VALUES
    ('ag-0000000000000001', 's-0000000000000001', 'Metro', 'agent', 'The default agent.',
     '["workspace.list","workspace.read","workspace.check","workspace.mcp_list","workspace.mcp","threads"]',
     1, '#4f46e5', 0, 0),
    ('ag-0000000000000002', 's-0000000000000001', 'Narrowed', 'narrowed', 'No workspace MCP calls.',
     '["workspace.list","workspace.read","workspace.check","workspace.mcp_list","threads"]',
     0, '#0891b2', 1, 1),
    -- A partial upgrade with write already enabled and in a non-default position.
    ('ag-0000000000000003', 's-0000000000000001', 'Writer', 'writer', 'Already has write.',
     '["workspace.list","workspace.write","workspace.read","workspace.check","workspace.mcp_list","workspace.mcp","threads"]',
     0, '#059669', 2, 2);
