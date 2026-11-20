# Extra runs (DURATION=5s, c=64)

## html_nohead_1m.html (HTML evidence, no <head>: the library scans the whole document)
| server | page | conns | req/s off | req/s on (Δ) | req/s on-mem (Δ) | p50 ms off/on/on-mem | p99 ms off/on/on-mem | CPU µs/req off/on/on-mem | errors |
|---|---|---:|---:|---:|---:|---|---|---|---|
| httpd | html_nohead_1m.html | 64 | 42805 | 1556 (-96.4%) | 1667 (-96.1%) | 0.85 / 28.32 / 25.42 | 4.35 / 391.41 / 216.69 | 36.1 / 1280.1 / 1199.2 |  |
| nginx | html_nohead_1m.html | 64 | 46232 | 1529 (-96.7%) | 1545 (-96.7%) | 0.76 / 36.05 / 40.12 | 1.42 / 385.59 / 128.89 | 24.0 / 1309.8 / 1294.5 |  |

## httpd with EnableMMAP Off
| server | page | conns | req/s off | req/s on (Δ) | req/s on-mem (Δ) | p50 ms off/on/on-mem | p99 ms off/on/on-mem | CPU µs/req off/on/on-mem | errors |
|---|---|---:|---:|---:|---:|---|---|---|---|
| httpd | html_512.html | 64 | 106846 | 110682 (+3.6%) | 113217 (+6.0%) | 0.53 / 0.49 / 0.48 | 2.80 / 2.56 / 2.54 | 17.0 / 16.5 / 16.3 |  |
| httpd | html_16k.html | 64 | 115230 | 105303 (-8.6%) | 105088 (-8.8%) | 0.49 / 0.57 / 0.50 | 2.40 / 2.73 / 2.68 | 15.6 / 17.6 / 17.8 |  |
| httpd | html_100k.html | 64 | 116375 | 98781 (-15.1%) | 100874 (-13.3%) | 0.46 / 0.57 / 0.54 | 2.44 / 2.89 / 2.69 | 16.0 / 18.9 / 18.3 |  |
| httpd | html_1m.html | 64 | 44545 | 44908 (+0.8%) | 42871 (-3.8%) | 0.96 / 0.93 / 0.95 | 3.72 / 3.31 / 6.35 | 33.3 / 35.4 / 42.0 |  |
