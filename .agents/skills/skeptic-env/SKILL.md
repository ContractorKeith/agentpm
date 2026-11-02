---
name: skeptic-env
description: agentpm-specific acceptance environment for independent review.
---

# agentpm review environment

Read AGENTS.md, docs/PLAN.md, CONTEXT.md, and the complete branch diff. Run the documented Cargo gate once M0 exists. Use a temporary AGENTPM_DATA_DIR and temporary repositories; never run destructive scenarios against user projects or real credentials.

Attack external-effect reconciliation, stale head/base evidence, duplicate requests, process ownership, context provenance, integration-base rebuilds, human takeover, MCP authority, and budget persistence. Use deterministic fake agents/host services for ordinary checks. Live backend smoke tests are explicit acceptance evidence and must identify backend version and OS; absence is not a pass. Report severity, reproducer, expected/actual behavior, and exact tested SHA. Do not merge, tag, publish, or alter acceptance requirements.
