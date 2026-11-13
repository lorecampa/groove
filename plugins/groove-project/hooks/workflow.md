This repository has enabled the Groove project workflow in its local Git config.
For code, test and documentation changes, load and follow the Groove skill
(groove:groove in Claude Code). A coordinator reserves a pool worktree before
task edits, builds, tests and commits. Assigned workers reuse their supplied
pool, lease ID and worktree; they do not acquire or release another reservation.
Read-only investigation and coordination do not need a new reservation.
Keep the reservation identifiers outside the reusable checkout. Stop all writers
and preserve results before the coordinator releases its reservation.
Use the same explicit pool on every pool command. Follow the skill's recovery
rules on failures; never bypass protection or discard another task's work.
The harness handles delegation and communication. Installing the plugin does not
authorize pushing, merging or changing repository policy.
