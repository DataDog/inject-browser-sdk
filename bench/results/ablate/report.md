A = `HEAD`, B = `worktree`

| config | A ns/page | B ns/page | B vs A | layout spread A / B | verdict |
|---|---:|---:|---:|---:|---|
| head_1k_16k_whole | 1464 | 1044 | -28.7% | 5.0% / 2.5% | faster |
| head_8k_16k_whole | 1452 | 1040 | -28.4% | 4.4% / 4.5% | faster |
| head_late_16k_whole | 2554 | 209 | -91.8% | 4.3% / 6.1% | faster |
| head_tiny_16k_whole | 1094 | 236 | -78.4% | 10.6% / 5.9% | faster |
| no_head_16k_whole | 28125 | 137 | -99.5% | 10.2% / 7.3% | faster |
| not_html_16k_whole | 13581 | 40 | -99.7% | 3.2% / 7.2% | faster |
