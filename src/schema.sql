CREATE TABLE projects (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 128),
 repository TEXT NOT NULL UNIQUE
);
CREATE TABLE runs (
 id TEXT PRIMARY KEY NOT NULL,
 project_id TEXT NOT NULL REFERENCES projects(id),
 state TEXT NOT NULL CHECK(state IN ('queued','running','waiting','paused','completed','failed','canceled')),
 protocol_version INTEGER NOT NULL CHECK(protocol_version = 1),
 created_at TEXT NOT NULL,
 usage_json TEXT NOT NULL DEFAULT '{"status":"unknown"}' CHECK(json_valid(usage_json)),
 executing_milliseconds INTEGER NOT NULL DEFAULT 0 CHECK(executing_milliseconds >= 0),
 repair_count INTEGER NOT NULL DEFAULT 0 CHECK(repair_count >= 0),
 replan_count INTEGER NOT NULL DEFAULT 0 CHECK(replan_count >= 0)
);
CREATE TABLE milestones (
 id TEXT PRIMARY KEY NOT NULL,
 run_id TEXT NOT NULL REFERENCES runs(id),
 state TEXT NOT NULL CHECK(state IN ('queued','implementing','checking','reviewing','repairing','stale_base','ready','human_owned','waiting','paused','completed','failed','canceled'))
);
CREATE TABLE events (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
 run_id TEXT NOT NULL REFERENCES runs(id),
 request_id TEXT NOT NULL,
 protocol_version INTEGER NOT NULL CHECK(protocol_version = 1),
 created_at TEXT NOT NULL,
 payload TEXT NOT NULL CHECK(json_valid(payload))
);
CREATE TABLE requests (
 request_id TEXT PRIMARY KEY NOT NULL,
 command_json TEXT NOT NULL CHECK(json_valid(command_json)),
 response_json TEXT CHECK(response_json IS NULL OR json_valid(response_json))
);
CREATE TABLE evidence (
 id TEXT PRIMARY KEY NOT NULL,
 milestone_id TEXT NOT NULL REFERENCES milestones(id),
 head TEXT NOT NULL CHECK(length(head) > 0),
 base TEXT NOT NULL CHECK(length(base) > 0),
 context_id TEXT NOT NULL CHECK(length(context_id) > 0),
 prerequisite_heads TEXT NOT NULL CHECK(json_valid(prerequisite_heads)),
 check_configuration TEXT NOT NULL,
 implementer_session TEXT NOT NULL,
 reviewer_session TEXT NOT NULL CHECK(reviewer_session <> implementer_session),
 payload TEXT NOT NULL CHECK(json_valid(payload))
);
