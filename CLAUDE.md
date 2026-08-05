<!-- moat:onboarded -->
## Moat ship loop (BINDING)

This repo ships through the **moat acceptance loop** — no change is done without
a mechanically-produced receipt. The full loop, before you touch code:

```
0a. FUZZY ask (a vibe, "make X better", unstated shape)? GRILL it into a
    spec first — eagerly. Do the brownfield homework, then interrogate the
    forks until the ask is one specific job. Cheapest point to get it right.
0b. PLAN-shaped (intent / PRD / multi-step)? GROUND it: run the blind-spot
    pass (moat dialectic_run / tribunal) before contracting; engage a must_ask.
1.  moat contract --repo . --title "..." --ac "<criterion>" --touch <path>
2.  build the change (worktree recommended)
3.  validate, then commit normally; the pre-commit hook checks packet coverage
4.  MODE FIRST: is this pearl under an active Conductor epic?
    IF CONDUCTED:
      moat ready <pearl> --worktree <path> --epic <epic>
      STOP. The ready SHA is frozen. Do NOT commit, rebase, submit, close, push,
      move the branch, write main, or run conduct after ready. The operator/
      Conductor runs moat conduct <epic>.
    ELSE STANDALONE:
      moat submit --packet <p> --workspace .
      ACCEPTED -> close work, push (pre-push checks HEAD coverage)
      REJECTED -> fix the NAMED violation, never weaken a gate
      INSTRUMENT INVALID -> stop, report; trust nothing from the run
      ABSTAINED -> honorable; routes to the operator queue
```

Gates are mechanical walls: tests pass in a clean sandbox, created tests must
fail at baseline, test-value floor, and — if this repo is thermo-calibrated —
the **thermo floor** rejects a change that makes a function MORE complex past the
repo threshold (simplify it, don't override). The thermo ceiling (LLM structural
review) is advisory.

Full playbook: `moat skill --print`. Per-session refresher: `moat prime`.
<!-- /moat:onboarded -->


## Snag reporting

Use `snag` to capture material out-of-scope bugs, friction, or papercuts.
Report when unexpected, costly, and plausibly systematic. Do not report ordinary
failures or mistakes. Before completing a task, review for snags.

`snag report "<title>" --kind <kind> --severity <minor|major|blocker> --observed "<what>"`

Context is automatic. Full rubric: global `~/.claude/CLAUDE.md`.
