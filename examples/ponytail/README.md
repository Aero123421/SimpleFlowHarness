# SFH × Ponytail-inspired Harness Flow Pack

`sfh v1.6`向けの、Ponytailの思想をプロンプトとループ設計へ落とし込んだ20本のサンプルフローです。

これはPonytail本体の再配布ではなく、公開されている思想を要約・再構成した**非公式の参考例**です。
Ponytailがインストールされていなくても、`prompts/`のcontextだけで動作するようにしています。

## 共通思想

- 理解してから最小化する。
- 新しく作る前に、不要・既存実装・stdlib・native機能・導入済み依存を確認する。
- bugは症状ではなく共有root causeを直す。
- 非自明な変更には最小のrunnable checkを残す。
- security、trust-boundary validation、data-loss prevention、accessibility、明示要件は削らない。
- AIの自己申告よりworkspaceとdeterministic commandを信じる。
- planner / builder / evaluatorを分離し、reviewer同士も独立させる。
- loopはboundedにし、判断不能は`stuck`へ送る。
- writer flowは原則1 run 1 managed worktree。
- 長時間flowはartifact handoff、context上限、budget landingを持つ。
- 一度に全部掃除せず、1 runで1 coherent cutを基本にする。

## 導入

これらのflowはsfh repositoryの`examples/ponytail/`にあります。単体でも動くので、
このdirectoryごと自分のprojectへcopyしても構いません。

**writer flowは対象projectのGit repository rootから起動してください。** flow file自身の
場所とrunの起点は別物です。`inputs/`と`prompts/`はflow fileからの相対で解決されるので、
どこから起動してもcontextは同じものが渡ります。

```bash
cd /path/to/target-project
PACK=/path/to/SimpleFlowHarness/examples/ponytail

sfh validate "$PACK/01-yagni-first-minimal-feature.yaml" \
  --profiles "$PACK/profiles.codex-pi.example.yaml"

sfh plan "$PACK/01-yagni-first-minimal-feature.yaml" \
  --profiles "$PACK/profiles.codex-pi.example.yaml" \
  --state-dir ~/.local/state/sfh --json --save

sfh run "$PACK/01-yagni-first-minimal-feature.yaml" \
  --profiles "$PACK/profiles.codex-pi.example.yaml" \
  --state-dir ~/.local/state/sfh --detach --json
```

1. `inputs/`の該当fileを編集する。
2. project固有のtest/lint commandへ必要に応じて置換する。
3. `validate --strict`、`preflight`、`plan --json`を確認する。
4. その後にrunする。

`inputs/`をそのままcommitせずに使いたい場合は、packをcopyしてからそのcopy側を編集して
ください。sfhはcontext fileをrun開始時にsnapshotするので、run中にpackを書き換えても
そのrunへは反映されません（v1.5.0のprovenance修正）。

## Profile

各flowには、そのままvalidateできるinline profileがあります。

- planner / analyst / reviewer: Codex read
- builder / maintainer: Pi write
- security reviewer: Claude read
- builder fallback: Codex write

`--profiles` overlayでtool、model、bin、effortを差し替えられます。
profile名は固定役職ではなく、単なるサンプル名です。

## Workspace

- writerを含むflowは`workspace.mode: auto`により原則1個のmanaged worktreeを作ります。
- loopやvisitが増えてもstepごとにworktreeは増えません。
- review-only flow（08、09）は`workspace.mode: current`で既存diffを読みます。
- database migrationとrelease flowは`cleanup: keep`で、人間の確認前にworkspaceを消しません。
- これらのflowはmerge、tag、publish、production migrationを自動実行しません。

## Context

各flow fileと同じdirectory以下の`inputs/`と`prompts/`をnamed contextとして使用します。
sfhはbundleとhashをrun artifactsへ残します。

Ponytailをnative Skillとして導入済みの場合、tool側で追加activationしても構いません。
ただし、このpackはportable contextを既に渡すため、同じ長文rulesetを二重に注入しない方がcontext効率は良いです。

## Deterministic checks

汎用flowの一部はportableな最低限として`git diff --check`だけを置いています。
対象projectに合わせ、必ず実際のbuild/test/lint commandを追加または置換してください。

言語別flowには以下を例示しています。

- Rust: `cargo fmt` / `cargo clippy` / `cargo test` / `cargo package`
- Python: `python -m pytest`
- Node/frontend: `npm run lint` / `npm test`
- Migration: `python tools/migrate.py --dry-run`（project側へ合わせて置換）

## 20 flows

| # | File | 主なpattern |
|---:|---|---|
| 01 | `01-yagni-first-minimal-feature.yaml` | そもそも実装が必要か判定してから最小feature |
| 02 | `02-rust-root-cause-bugfix.yaml` | Rust root-cause bugfix、regression-first |
| 03 | `03-python-regression-first-bugfix.yaml` | Python pytest regression-first |
| 04 | `04-rust-dependency-elimination.yaml` | dependencyをstdlib/既存実装へ置換 |
| 05 | `05-frontend-native-first.yaml` | semantic HTML/CSS/browser native first |
| 06 | `06-rust-cli-simplification.yaml` | CLI surfaceとmachine contractを小さく保つ |
| 07 | `07-python-trust-boundary-hardening.yaml` | 最小化してもsecurity boundaryは削らない |
| 08 | `08-existing-diff-dual-review.yaml` | correctness + simplicityの独立review |
| 09 | `09-existing-diff-three-way-council.yaml` | correctness + security + simplicity council |
| 10 | `10-whole-repo-ponytail-audit.yaml` | whole-repo read-only complexity audit |
| 11 | `11-audit-then-one-safe-cut.yaml` | audit後に1件だけ安全に削る |
| 12 | `12-docs-code-drift-repair.yaml` | repository knowledgeをactual behaviorへ同期 |
| 13 | `13-planner-builder-evaluator-loop.yaml` | planner / generator / evaluator bounded loop |
| 14 | `14-sequential-chunked-long-build.yaml` | foreachを直列化した長時間build、compact handoff |
| 15 | `15-production-failure-to-eval-fix.yaml` | production evidence → eval → fix → regression |
| 16 | `16-flaky-test-root-cause.yaml` | sleep/retryで隠さずflakeの原因を除去 |
| 17 | `17-database-migration-dry-run.yaml` | migration dry-run後にhuman gate (`stuck`) |
| 18 | `18-profiler-gated-performance-fix.yaml` | measured bottleneckがある時だけ最適化 |
| 19 | `19-ponytail-debt-garbage-collection.yaml` | `ponytail:` ceilingを定期的に1件処理 |
| 20 | `20-rust-release-readiness-loop.yaml` | deterministic release gates、publishはしない |

## Harness engineeringとの対応

最近のagent-first engineeringで繰り返し現れる設計を、次のように反映しています。

- depth-first decomposition: 01、02、13、14
- worktree-per-change / isolated environment: writer flow全般
- plans and handoffs as first-class artifacts: 13、14、20
- generator/evaluator separation: 08、09、13、20
- bounded review-correction loops: 01〜07、11〜20
- deterministic eval and regression evidence: 02、03、04、06、07、15、16、20
- production correction to reusable eval: 15
- continuous entropy garbage collection: 10、11、19
- human escalation for irreversible or ambiguous work: 17、20
- progressive disclosure through named context: 全flow
- blast-radius reduction: managed workspace、read-only reviewers、no auto-publish

## Validation

`VALIDATION.md`を参照してください。20本すべてが実物の`sfh validate`を通ることをCIで
継続的に確認しています。

実際のAI CLI、project command、OS依存挙動は対象環境で`sfh preflight`と`sfh doctor`を実行してください。
