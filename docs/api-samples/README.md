# Sonar API samples

Raw responses captured by `sonar-technician probe` on the user's machine. These
are the source of truth for `src/sonar/models.rs`. Don't edit the JSON by hand.

- `classic/`: probe run with Sonar in Classic mode
- `stream/`: probe run with Sonar in Streamer mode

Each folder holds `<endpoint>.json` (raw body), `<endpoint>.meta.txt`
(URL, status, timing), `subApps.json`, `coreProps.json` and `summary.txt`.
Note the GG version (GG → Settings → About) in the commit message when
adding or refreshing samples.
