# Reviewed project preparation and commands

Only run the project's reviewed `.groove.toml` setup when the assignment calls
for it. Use `acquire --setup PROFILE` or `exec --lease-id ID --setup PROFILE`,
with the same explicit pool selection. Setup never runs implicitly.

Check preparation status before starting dependent work. A setup failure can
leave an acquired reservation protected: retain identifiers from the error and
inspect it. Do not acquire a replacement slot. Failed, interrupted or changed
setup requires an explicit `--retry`, which restarts from the first step, after
stopping/recovering writers. Keep results outside approved cache roots, because
setup may clear those roots.

`exec --recipe NAME` and `exec --agent NAME -- ARGS` append literal arguments.
Required setup is checked but never run automatically. Built-in agent names
only select an executable; pass its noninteractive arguments or use a reviewed
project adapter. Do not enable native agent worktree hooks that delete or
recreate Groove's permanent slots.

A worker launched by `exec --agent` stays inside its assigned reservation and
hands results back. The coordinator owns finalization after execution ends.
