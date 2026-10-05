# Scale benchmark

`generate.py` writes an Icinga 2 config with N hosts × 15 services (about 5% problems, realistic outputs, perfdata and vars). `benchmark.sh` starts a real Icinga 2.15 in Docker with it and measures load sizes and times and a forced re-check burst on the event stream. The results and the design they led to are in `docs/performance.md`.
