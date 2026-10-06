# Scale benchmark

`generate.py` writes an Icinga 2 config with N hosts × 15 services (about 5% problems, realistic outputs, perfdata and vars). `benchmark.sh` starts a real Icinga 2.15 in Docker with it and measures load sizes and times and a forced re-check burst on the event stream. The results and the design they led to are in `docs/performance.md`.

`starts.sh [hosts] [clients]` (default 2000 and 20) starts the same Icinga in a container of its own and measures many icygui engines starting at once, first as users opening the app, then (on the restarted Icinga) as background starts with their size-proportional wait (PERF-09): when they were connected and the master's peak memory (`docker stats`). It runs `ic-core`'s ignored `starts` test, which refuses anything but that local container, and removes the container and its volume at the end. Results in `docs/performance.md` (*Quiet mode and load pacing*).
