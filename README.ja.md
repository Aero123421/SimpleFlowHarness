# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | 日本語

AIコーディングCLI — **Codex**、**Claude Code**、**opencode**、**Grok**、**Antigravity (`agy`)**、**Pi**、**Cursor** — と任意のシェルコマンドを、YAMLで定義した多段フローにつなげて実行する単一バイナリのランナーです。条件分岐、リトライ、並列実行、中断からの再開、実行ログの保存を担います。

sfh はプロセスの起動・ルーティング・記録・復旧だけを行い、何をするかの判断は各コマンドとエージェントに任せます。ステップの成否は終了コードと機械可読プロトコルで判定し、文章の読み取りには頼りません。

**目次** — [できること](#できること) · [インストール](#インストール) · [クイックスタート](#クイックスタート) · [フロー形式](#フロー形式まとめ) · [終了コード](#終了コード) · [プログラムからの実行](#プログラムからの実行) · [実行後に残るもの](#実行後に残るもの) · [ドキュメント](#ドキュメント)

## できること

| 機能 | 仕組み |
|---|---|
| 条件分岐 | `route:` で最終行・プロトコル状態・ラベル・並列メンバーの結果を見て `goto:end` / `fail` / `stuck` へ |
| 並列実行 | `parallel`（異種ワーカー同時実行）と `foreach`（行・JSON配列での動的展開） |
| バックグラウンド実行 | `sfh run --detach` して `status` / `wait` / `stop` で操作 |
| 中断からの再開 | `--resume-latest`。実行Closure（フロー・コンテキスト・ツールバージョン・ワークスペース）が開始時と変わっていれば再開を拒否 |
| マネージド・ワークスペース | `workspace: {mode: git-worktree}` で書き込みは `sfh/<flow>/<run-id>` ブランチのworktreeへ。本体のcheckoutは触らず、未コミット変更を自動で破棄しない |
| 予算制限 | `max_cost_usd`、`wall_clock_sec`、`max_visits`、`max_total_steps`。フロー自体の修正時は `--carry-budget-from` で消費分を引き継ぎ |
| アクセス範囲 | AIステップは `access: read \| write \| full` の明示が必須 |
| 機械可読インターフェース | `--json` で `run`/`plan`/`wait`/`stop`/`status`/`preflight`/`workspaces` がエンベロープを返す。エラーは安定した `SFH_*` コード |
| 無料の事前チェック | `sfh preflight` はモデルを呼ばずにバイナリ・バージョン・フラグを検証。`sfh doctor` は1トークンだけ実際に投げてプロトコルずれを検出 |

## インストール

**macOS / Linux:**

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.ps1 | iex
```

**Homebrew:**

```bash
brew install Aero123421/tap/sfh
```

ビルド済みバイナリとSHA-256チェックサムは [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)。バージョン固定は `SFH_VERSION`、インストール先の変更は `SFH_INSTALL_DIR` / `SFH_DATA_DIR`、PATH書き換えの抑止は `SFH_NO_MODIFY_PATH=1`。各インストール方法が何を検証し、何を信頼しているかは [docs/distribution.md](docs/distribution.md) にあります。

## クイックスタート

テストを実行し、失敗したらAIエージェントに修正させるループ（`max_visits: 3` で打ち切り）:

```yaml
api_version: 1
name: test_and_repair
defaults:
  max_visits: 3
  wall_clock_sec: 1800
workspace:
  mode: git-worktree

steps:
  - id: test
    cmd: ["cargo", "test"]
    effects: workspace
    on_error: goto:fix

  - id: ship
    cmd: ["sfh", "--version"]
    effects: read
    route: [{goto: end}]

  - id: fix
    tool: codex
    access: write
    prompt: |
      このstderrファイルを開き、失敗原因を特定して修正してください:
      {{steps.test.stderr_file}}
    route: [{goto: test}]
```

```bash
sfh validate flow.yaml --strict   # 構文・変数・ルートの静的検証
sfh plan flow.yaml                # 使い捨ての一時ディレクトリで実行計画を確認
sfh run flow.yaml                 # 実行
```

さらなる例は [examples/](examples/)（`research.yaml`、`parallel-ideas.yaml`、`managed-loop.yaml` と、実リポジトリ作業向けのフロー20本を収録した [examples/ponytail/](examples/ponytail/)）。

## フロー形式まとめ

ステップは2種類:

| 種類 | 書き方 | 成否の判定 |
|---|---|---|
| AIツール・ステップ | `tool: codex`（＋`access`、`prompt`） | ツールの機械可読プロトコル（fail-closed。出力形式がずれればそのステップは失敗） |
| シェルコマンド | `cmd: ["cargo", "test"]`（配列ならシェル経由なし、文字列なら `sh -c` / `cmd /C`） | 終了コード。`outcomes:` で意味を割り当てることも可能 |

AIステップは既定で `allow_empty: false` です。最終メッセージなしに終わったターンは失敗扱いになります。成果物が差分であるワーカーには `allow_empty: true` を指定し、検証は後続の `cmd:` ステップに任せてください。

どのステップでも使えるルーティング条件:

| 条件 | 判定内容 |
|---|---|
| `when_last_line_is: X` | 出力の最終行が厳密に一致 |
| `when_protocol_is:` `plain / valid / missing_terminal / invalid` | 失敗したリーフのプロトコル状態（`on_error: continue` が前提） |
| `when_label_is: X` | `outcomes:` で終了コードに付けたラベル |
| `when_members: {last_line_is: X, all/n: ...}` | `parallel` / `foreach` メンバーの多数決 |

最終行で分岐するレビューステップ:

```yaml
api_version: 1
steps:
  - id: review
    tool: claude
    access: read
    prompt: "変更をレビューし、最後に PASS または REVISE とだけ書いてください。"
    route:
      - {when_last_line_is: PASS, goto: end}
      - {when_last_line_is: REVISE, goto: stuck}
      - {goto: stuck}
```

2つのエージェントが並列レビューし、全員一致のPASSのみ通過:

```yaml
api_version: 1
steps:
  - id: council
    max_parallel: 3
    parallel:
      - {id: rev_a, tool: claude, access: read, on_error: continue, prompt: "最後に PASS または FAIL とだけ書いてください。"}
      - {id: rev_b, tool: codex, access: read, on_error: continue, prompt: "最後に PASS または FAIL とだけ書いてください。"}
    route:
      - {when_members: {last_line_is: PASS, all: true}, goto: end}
      - {goto: fail}
```

その他の機能 — セッション継続（`continue_from` / `fork_from`）、中断ステップの扱い（`replay.unfinished`）、コンテキスト固定（`contexts:`）、ツールバージョン指定（`require_version`）、プロファイルの上書き（`--profiles`）、終了コードとプロトコルの矛盾（`exit_conflict`） — は `sfh guide`、[CHANGELOG.md](CHANGELOG.md)、スキーマを参照してください。

## 終了コード

| コード | 意味 |
|---|---|
| 0 | `goto:end` で正常終了、または状態が `done` |
| 1 | 失敗（`goto:fail`）、ツールエラー、または `failed` / `dead` / `stopped` |
| 2 | 設定・CLI引数・静的検証のエラー |
| 3 | 実行中（`status`、またはタイムアウトした `wait`） |
| 4 | `goto:stuck` — 人間の判断を待っている状態 |

## プログラムからの実行

```bash
sfh preflight flow.yaml --json          # モデル呼び出しなしの事前チェック
sfh plan      flow.yaml --json --save   # 実行計画のみ。何も起動しない
sfh run       flow.yaml --json --detach # ハンドルとnext_actionsを返す
sfh wait <run-dir> --json               # 完了まで待って結果を返す
```

stdout にはJSONエンベロープのみが出力され（進行状況はstderrへ）、失敗時はメッセージではなく安定したエラーコード（`SFH_USAGE`、`SFH_STEP_FAILED`、`SFH_PROTOCOL_INVALID`、`SFH_EXECUTION_CLOSURE_CHANGED` など）で分岐できます。詳細な仕様は [docs/machine-api.md](docs/machine-api.md)。

## 実行後に残るもの

`.sfh/runs/<run-id>/` に追記専用の記録が残ります: `log.jsonl`（イベント・トークン数・コスト・プロトコル証跡）、上限付きの `<step>.out.txt` / `.err.txt`（32 MiB、冒頭と末尾を保存）、`status.json`、`execution-closure.json`、ワークスペースとコンテキストのスナップショット。再開時はこのログから判断を再生するため、完了済みステップを実行し直すことはありません。

公開JSONスキーマ: [フロー](schema/flow.schema.json) · [ログイベント](schema/log-event.schema.json) · [状態](schema/status.schema.json) · [保持ポリシー](schema/retention.schema.json)

## ドキュメント

- `sfh guide` — 内蔵の文法リファレンス（AIにフローを書かせるときにも読ませてください）
- `sfh --help`、`sfh <command> --help` — CLIオプション
- [docs/README.md](docs/README.md) — 現行ドキュメントと過去資料の索引
- [skills/](skills/) — sfhの設計ルールをAIに教える [Agent Skills](https://agentskills.io/specification)（`cp -R skills/sfh-* .agents/skills/` で導入。実行時機能ではありません）
- [AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md) · [CHANGELOG.md](CHANGELOG.md)

対応プラットフォーム: Windows / macOS / Linux
