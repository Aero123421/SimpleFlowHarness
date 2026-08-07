# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | 日本語

`sfh` は、AIコーディングCLIや任意のシェルコマンドをYAML定義の多段フローとしてオーケストレーションする軽量な単一バイナリのワークフローランナーです。

**Codex**、**Claude Code**、**opencode**、**Grok**、**Antigravity (`agy`)**、**Pi**、**Cursor** などのAIツールや任意のローカルコマンドを接続し、条件分岐・リトライ・並列実行・セッション継続・耐久再開・監査ログ保存を制御します。

エンジンはプロセス実行、ルーティング、ログ記録の配管に専念し、タスクの合意形成や結果の評価はコマンドおよびAIエージェント自身が行います。

---

## なぜ sfh なのか？

- **コンテキストの保護**: 多段のエージェント管理ループをメインAIエージェントのコンテキストウィンドウから分離。
- **クリーンな出力と完全な監査**: 進捗は `stderr`、選択した結果は `stdout` へ出力。プロンプト、各ステップの出力、トークン数、報告コスト、イベントログを `.sfh/runs/` 以下に保存。
- **バックグラウンド実行**: 長時間フローを切り離して実行 (`--detach`)。進捗確認 (`sfh status`)、結果回収 (`sfh wait`)、安全停止 (`sfh stop`) を独立して実行可能。
- **並列処理と再開機能**: 異種ツールの並列実行 (`parallel`) や動的ループ (`foreach`) に対応。中断・クラッシュした実行を完了済みステップを再実行せずに再開 (`--resume`)。
- **安全制御と予算制限**: ツールが報告したコストのソフト上限 (`max_cost_usd`)、実行時間上限 (`wall_clock_sec`)、明示的なアクセス権限 (`access: read | write | full`) による安全運用。

---

## インストール

### 公式ワンラインインストーラー（パッケージマネージャー不要）

**macOS / Linux:**
```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.sh | sh
```

**Windows PowerShell:**
```powershell
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.ps1 | iex
```

OSおよびCPUアーキテクチャを自動判定し、SHA-256検証、展開、`PATH` 設定までを行います。
実行前に [Shell版](installers/sfh-installer.sh) および [PowerShell版](installers/sfh-installer.ps1) の内容を確認できます。
バージョン固定や設定変更オプション:
- `SFH_VERSION=1.1.5`: 特定のバージョンに固定。
- `SFH_INSTALL_DIR=/path/to/bin`: インストール先ディレクトリの指定。
- `SFH_NO_MODIFY_PATH=1`: 永続的な `PATH` 変更を無効化。

### パッケージマネージャー・手動ダウンロード

**Homebrew (macOS / Linux):**
```bash
brew install Aero123421/tap/sfh
```

手動・オフライン導入用のバイナリおよびSHA-256ハッシュは [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest) から取得可能です。

---

## クイックスタート

テスト実行とAIによる自動修正を組み合わせた最小構成のフロー (`flow.yaml`) 例:

```yaml
api_version: 1
name: test_and_repair
defaults:
  max_visits: 3
  wall_clock_sec: 1800

steps:
  - id: test
    cmd: ["cargo", "test"]
    on_error: goto:fix

  - id: ship
    cmd: ["sfh", "--version"]
    route: [{goto: end}]

  - id: fix
    tool: codex
    access: write
    prompt: |
      次のstderrファイルを開き、原因を診断して修正してください:
      {{steps.test.stderr_file}}
    route: [{goto: test}]
```

### 検証と実行

```bash
# 1. 構文・変数・ルーティングの静的検証
sfh validate flow.yaml --strict

# 2. 隔離された一時ディレクトリで実行計画を事前に確認
sfh plan flow.yaml

# 3. フローの実行
sfh run flow.yaml
```

---

## メンタルモデルと主要機能

### ステップ種別とAIツール

`sfh` では2つのステップ形式を使用します:
1. **AIツールステップ**: `codex`、`claude`、`opencode`、`grok`、`agy`、`pi`、`cursor` の組み込みプリセット。アクセス権限 (`access: read | write | full`) の宣言が必須です。
2. **確定コマンド (`cmd`)**: 配列形式 (`["cargo", "test"]`) でシェルを介さず直接起動します。文字列形式は `sh -c` (Unix) や `cmd /C` (Windows) で実行されます。長いプロンプトは `{{prompt_file}}` 経由で安全に受け渡せます。

### 制御フローとルーティング

各ステップは `route` 規則を上から順に評価し、次に遷移するステップを決定します:
```yaml
api_version: 1
steps:
  - id: review
    tool: claude
    access: read
    prompt: "コード変更をレビューし、最終行に PASS または REVISE と出力してください。"
    route:
      - {when_last_line_is: PASS, goto: end}
      - {when_last_line_is: REVISE, goto: stuck}
      - {goto: stuck}
```
遷移先として `<step-id>`、`goto:end` (正常終了, exit 0)、`goto:fail` (異常終了, exit 1)、`goto:stuck` (人間介入待ち, exit 4) を指定できます。

### 並列実行と合意形成

- **`parallel`**: 複数の異種ステップを同時実行。
- **`foreach`**: 行分割やJSON配列に基づく動的ファンアウト (`split: lines | json`)。
- **`when_members`**: 正常終了 (`exit == 0`) かつ最終行が一致するメンバーの票数をカウントして判定:

```yaml
api_version: 1
steps:
  - id: council
    max_parallel: 3
    parallel:
      - {id: rev_a, tool: claude, access: read, on_error: continue, prompt: "最終行に PASS または FAIL と出力。"}
      - {id: rev_b, tool: codex, access: read, on_error: continue, prompt: "最終行に PASS または FAIL と出力。"}
    route:
      - {when_members: {last_line_is: PASS, all: true}, goto: end}
      - {goto: fail}
```

### セッションの継続と分岐

- **`continue_from: step_id`**: 単一のサーバー側セッションを前ステップから継続。
- **`fork_from: step_id`**: 親ステップのセッション文脈を保持したまま独立した子セッションへ分岐 (`claude`, `opencode`, `grok`, `pi` で対応)。

### バックグラウンド実行と運用操作

ターミナルを占有せずに長時間タスクを実行可能:
```bash
# バックグラウンド実行（実行ディレクトリを表示して即終了）
sfh run flow.yaml --detach

# パス省略時は最新の実行が対象
sfh status --json

# 終了まで待機し結果を stdout で取得
sfh wait --timeout 3600

# 実行の中止と子プロセスツリーの停止
sfh stop
```

### 耐久性と再開契約

実行履歴は `.sfh/runs/<run_dir>` に順次保存されます。エラーや停止が発生した場合、以下で再開できます:
```bash
sfh run flow.yaml --resume-latest
```
`sfh` はフロー定義と設定の整合性を検証し、完了済みステップをスキップして再開します。

### 終了コード

| 終了コード | 意味 |
| :---: | :--- |
| `0` | フローが正常完了 (`goto:end`) または `status` 観測結果が `done` |
| `1` | フロー失敗 (`goto:fail`)、ツールエラー、または `status` 観測結果が `failed` / `dead` / `stopped` |
| `2` | 設定エラー、無効なコマンドライン引数、または静的検証失敗 |
| `3` | フロー実行中 (`status` またはタイムアウトした `wait`) |
| `4` | `stuck` 状態に到達 (`goto:stuck`)、人間の介入が必要 |

---

## 成果物と公開スキーマ

すべての実行において `.sfh/runs/<run-id>/` 以下に耐久ログが保存されます:
- `log.jsonl`: 構造化イベントストリーム（ステップ開始・完了・トークン・コスト）
- `<step_id>.out.txt` & `<step_id>.err.txt`: サイズ制限付きのraw標準出力・標準エラー出力。32 MiBを超えるstreamは省略marker付きで先頭と末尾を保持し、構造化された最終回答とusage/costは完全なstreamから独立して処理します。
- `status.json`: リアルタイムステータススナップショット

コマンドの長い出力をAI promptへ渡す場合は、`{{steps.verify.output | tail:80 | truncate:8000}}`のように明示的に制限してください。全文artifactは`{{steps.verify.output_file}}`から参照できます。

公開 JSON スキーマ:
- [Flow JSON スキーマ](schema/flow.schema.json)
- [耐久ログイベント JSON スキーマ](schema/log-event.schema.json)
- [ステータススナップショット JSON スキーマ](schema/status.schema.json)

---

## 詳細情報とリソース

- 組み込み構文ガイド: ターミナルで `sfh guide` を実行
- CLIヘルプ: `sfh --help` または `sfh <command> --help`
- サンプルワークフロー: [examples/](examples/) ディレクトリ (`research.yaml`, `hypotheses.yaml`, `parallel-ideas.yaml`)
- ガイドライン・ポリシー:
  - [CONTRIBUTING.md](CONTRIBUTING.md)
  - [SECURITY.md](SECURITY.md)
  - [SUPPORT.md](SUPPORT.md)
  - [LICENSE](LICENSE)
  - [リリースページ](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)

対応プラットフォーム: **Windows**, **macOS**, **Linux**
