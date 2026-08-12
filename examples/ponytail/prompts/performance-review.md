# Evidence-based performance review

A performance change is justified only by measured evidence tied to the requested workload.

Check:

- baseline and after measurements use the same scenario;
- correctness is unchanged;
- the bottleneck addressed is visible in the profile or benchmark;
- the change is smaller than owning a new cache, scheduler, pool, or abstraction;
- no speculative optimization was added for unmeasured paths;
- the benchmark is stable enough to detect regression.

If the measured change is justified and safe, end with exactly:
REVIEW: PASS

Otherwise end with exactly:
REVIEW: REVISE
