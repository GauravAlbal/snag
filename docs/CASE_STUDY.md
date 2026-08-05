# Case study: 87 observations in one morning

*Anonymized account of the dogfood run that produced Snag's first corpus.
Aggregate numbers are from the real store; individual examples are sanitized —
tool names are genericized and no repository, path, model, or session
identifier appears.*

## The question

Would agents actually use an observation outbox? The tool only works if the
capture cost is near zero and the captured findings are worth keeping. So we
ran Snag against real agent work: one morning of ordinary multi-repository
development, with the agent instructed to record any material out-of-scope
issue while the evidence was fresh, then continue.

## The numbers

In about **2.5 hours** (one morning) across **5 repositories**:

| Metric | Value |
|---|---|
| Observations captured | 87 |
| Hash-chained records in store | 93 (87 creations + 6 retractions) |
| Observations later retracted | 4 |
| Captured by agents (`agent_report`) | 67 |
| Entered by a human (`human_explicit`) | 20 |
| Attached artifacts | 0 (capture stayed one command) |

Kind mix: bug 56, papercut 19, friction 6, usability 3, probe 2, feature 1.
Severity mix: blocker 1, major 29, medium 4, minor 38, low 15.

Honest framing: **87 observations is not a claim of 87 unique bugs.** The
corpus includes duplicates, probes, and later retractions — that is the point
of an outbox. The store survives them all; retraction is append-only, and
`snag verify --full` passed over the entire corpus.

## What the findings looked like

The findings clustered into six classes:

1. **Cancellation and recovery** (8 observations) — hung review steps with no
   timeout that stalled a pipeline; in-flight jobs that could not be aborted
   once started; no reap path for a job that outlived its caller.
2. **CLI contracts** — a documented flag that behaved like an input reader
   instead of an output mode; structured flags rejected in the fast-path form;
   a command that leaked internal identifiers in its normal output.
3. **Stale state** — a submission that inferred its baseline from dirty
   generated files and silently escalated the scope of the change.
4. **Cost controls** (4 observations) — spend ceilings and attribution absent
   from every product surface; the host's spend unreachable from the tool that
   was supposed to control it.
5. **Interface and documentation** (14 observations) — internal ontology
   surfaced on the happy path; required fields that a plain ticket could not
   satisfy; missing surfaces documented nowhere.
6. **Self-dogfood** (6 observations about Snag itself) — including the two CLI
   contract bugs above, which are **fixed in this release** (`report --json`
   output mode; structured flags on the fast path).

## Five sanitized examples

1. **`bug` / `major`** — "The submission step never infers its baseline when
   the workspace contains only generated files, so the diff collapses to
   bookkeeping and the scope check escalates to a human. Expected: generated
   dirt is excluded from the baseline inference." *(stale state)*
2. **`bug` / `major`** — "A long-running job cannot be aborted once in flight;
   the timeout leaves it running with no reap path. Expected: a cancel verb
   that terminates the job and settles its state." *(cancellation/recovery)*
3. **`papercut` / `minor`** — "The review step has no timeout, so a hung
   review blocks the pipeline indefinitely. Expected: a bounded deadline with
   an explicit outcome." *(cancellation/recovery)*
4. **`bug` / `minor`** — "A documented output flag behaves like an input
   reader: passing it with a title fails with a file-not-found error.
   Expected: output mode selects the response format." *(CLI contract)*
5. **`friction` / `minor`** — "The happy-path summary surfaces internal
   identifiers and ontology that a user never asked for. Expected: plain
   language on the happy path, internals behind a debug flag." *(interface)*

## What the run changed

- **Two confirmed fixes shipped** in Snag itself (examples 4 and its sibling:
  the fast-path structured flags), both caught by the dogfood corpus before any
  external user could hit them.
- **The rest were routed to their owning systems** — the run produced a
  triage-ready stream, not a memory.
- **Capture cost was near zero**: one command per finding, no context assembly,
  no tool switching. Context (repository, checkout, branch, HEAD) was attached
  automatically.

## The lesson

> Agents were already finding these issues. Snag changed whether the findings
> survived the session.

Before the outbox, an out-of-scope discovery was a judgment call: interrupt the
task to file it somewhere, or let it evaporate. With Snag, recording is one
command and the evidence outlives the session — including sessions that crash
or get killed. The corpus above would otherwise have been lost.
