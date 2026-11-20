# Offset-based `scan`: server cost, cutoff and TLS

Zen 4 host (c7a.8xlarge, `zen-bench`), servers pinned to cores 8,9, wrk to 24-27 (another L3 domain),
64 connections, one code layout per run (layout spreads were 1–3% where measured, far below the
differences that matter here). CPU µs per request, as a change over `off` (module not loaded).
Raw rows: the `*.jsonl` files in this directory, `harness/server_ablate_report.py` renders them.

Variants: `on` is the `Injector` (bytes handed back as slices); `scan` the `Scanner` (an offset,
headers held until the decision); `read`, `split`, `split_snippet` ablations without the library
(read 1 KiB; send it from memory and the rest by sendfile; the same with the snippet after it);
`-cork` keeps nginx's `tcp_nopush` on. `scan@<bytes>`: files up to that size read whole and sent
from memory, larger ones as what was read, the snippet, then sendfile.

## 1. First `scan`: sendfile on both sides of the snippet (`first-layout1.jsonl`)

Worse than the injector: nginx `html_16k` +70% (`on` +19%), httpd +60% (`on` +40%). Corking halved
every nginx variant (`scan-cork` +25%). The ablations put the cost on the serving pattern, not on
the library: splitting the file into two sendfile calls around a memory buffer costs a syscall
and a packet more than a memory prefix and one sendfile.

## 2. Memory prefix, small files whole: the cutoff

nginx, corked (`nginx-cutoff.jsonl`; the first sweep's nginx `scan@` figures are void: nginx
strips its workers' environment, fixed since):

| page | on-cork | scan, prefix | scan ≤4 KiB whole | scan ≤8 KiB whole | scan, always whole |
|---|---:|---:|---:|---:|---:|
| 2k | +17% | +17% | +1% | +1% | +1% |
| 4k | +16% | +16% | +5% | +5% | +5% |
| 8k | +16% | +17% | +16% | +14% | +14% |
| 16k | +15% | +15% | +14% | +15% | +30% |
| 100k | +13% | +13% | +12% | +12% | +155% |
| 1m | +4% | +6% | +5% | +4% | +574% |

httpd, reading with `pread` (`httpd-pread.jsonl`). Before that, APR read file buckets by mapping
them (`EnableMMAP On`), and reading 1 KiB alone cost +18% (`sweep-layout1.jsonl`); now +4%:

| page | on | read | split_snippet | scan, prefix | scan ≤8 KiB whole | scan, always whole |
|---|---:|---:|---:|---:|---:|---:|
| 512 | +27% | +4% | −4% | −0% | −0% | −0% |
| 2k | +39% | +4% | +6% | +9% | +1% | +1% |
| 4k | +38% | +4% | +6% | +9% | +2% | +1% |
| 8k | +38% | +4% | +6% | +9% | +5% | +4% |
| 16k | +39% | +4% | +6% | +9% | +10% | +12% |
| 100k | +36% | +3% | +5% | +8% | +9% | +78% |
| 1m | +19% | +2% | +3% | +5% | +5% | +423% |

The cutoff is 8 KiB on both servers, now the default: below it one read and one write beat
sendfile, and `scan` is at budget; past it, reading whole costs more and more.

## 3. HTTPS (`tls.jsonl`)

Without kTLS, neither server uses sendfile under TLS: files are read into memory to be encrypted,
so reading the first KiB costs nothing more, and encryption dominates the request.

| page | nginx on | nginx scan-cork | httpd on | httpd scan |
|---|---:|---:|---:|---:|
| 2k | +6% | +5% | +10% | −18% |
| 16k | +3% | +2% | +4% | −28% |
| 100k | +2% | +0% | +3% | −4% |
| 1m | +0% | −1% | +0% | −1% |

httpd `scan` looks cheaper than httpd without the module only because of httpd's default
`EnableMMAP On`: mod_ssl reads every file it encrypts through a fresh mapping, which `scan`'s
`pread` skips. Against `EnableMMAP Off` (`verify/tls-mmapoff.jsonl`), which makes httpd 18–20%
cheaper on its own, `scan` costs +4% at 2k, +6% at 16k and +2% at 100k, like nginx.

## 4. What a memory piece in front of the sendfile costs (`align.jsonl`, syscalls)

Syscalls per request (`harness/syscalls.sh`, nginx `html_32k`): `read-cork`, `split_snippet-cork`
and `scan` with a prefix make the same calls, a `pread`, a `writev`, a `sendfile` and the cork's
`setsockopt` pair; small files read whole drop the `sendfile` and the `setsockopt`s. What costs is
not a call more but the bytes sent from memory, copied twice (read, then write) where sendfile
copies none; splitting later costs more, page-aligned or not:

| page | read-cork | split at 1 KiB | at 4 KiB | at 8 KiB | split_snippet at 1 KiB | at 4 KiB |
|---|---:|---:|---:|---:|---:|---:|
| 16k | +7% | +10% | +18% | +26% | +12% | +20% |
| 32k | +6% | +9% | +16% | +26% | +11% | +18% |
| 100k | +5% | +7% | +14% | +20% | +9% | +15% |
| 1m | +2% | +4% | +2% | +6% | +1% | +5% |

About 0.6 µs a KiB from memory on this host. For a plaintext static page past 8 KiB the floor is
therefore: reading the 1 KiB window (+5–7%), sending it from memory (+3%), the snippet (+2%).
Sending the prefix by sendfile instead costs a second sendfile around the snippet, which section
1 measured dearer.

## Where it stands

* Within budget, or close: pages under TLS from about 100 KiB up (+4–6% below that); plaintext
  pages up to 4 KiB.
* Over budget: plaintext static pages past 8 KiB, sent by sendfile, at +12–17% on nginx and +8–9%
  on httpd. Reading the first KiB costs +4–6% of that and is unavoidable without knowing the file
  already (a decision cache keyed by inode, mtime and size, which the plan rules out); the rest is
  the bytes sent from memory rather than by sendfile, see section 4.

## Verification on a second host (`verify/`)

A fresh c7a.8xlarge, two code layouts per figure, same pinning. Plaintext reproduces within a
point: nginx `scan` +0% (2k), +6% (4k), +14% (8k), +15–16% (16–32k), +12% (100k), +4% (1m);
httpd `scan` +1%, +2%, +5%, +9%, +9%, +8%, +4%, `on` +37–39%. Under TLS, nginx came out a point
or two higher than the first run (2k +6%, 16k +4%, 100k +1%, 1m +1%), and httpd's 16k figure
against `EnableMMAP On` (−28% first, −15% here) does not reproduce; the comparison against
`EnableMMAP Off` above is the one that counts.
