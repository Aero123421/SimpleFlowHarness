# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**EN**: `sfh` chains AI coding CLIs — **Codex, Claude Code, opencode, Grok, Antigravity (`agy`)**, or any command — into YAML-defined multi-stage flows: review/retry loops, parallel fan-out, per-step model/effort/permission control, and cross-run **session resume**. It keeps your main agent's context window clean: stdout carries only the final step's output, everything else lands in a run directory. Single static binary for Windows / macOS / Linux. The docs below are in Japanese, but the YAML reference tables are language-neutral — and your favorite AI can translate the rest.

---

AI CLI(codex / claude / opencode / grok / agy / 任意コマンド)を **YAML定義の多段フロー** で非対話実行する小さなオーケストレータ。単一バイナリ、実行時依存なし、Windows / macOS / Linux 対応。

メインで使っているエージェント(例: codex app)からサブエージェント群を直接管理するとコンテキストが溶けるので、その管理ループを丸ごとこのCLIに追い出すのが目的。

- **stdoutには最終ステップの出力だけ**が出る(呼び出し元エージェントはそれだけ読めばいい)
- 全ステップのプロンプト・出力・ログは run ディレクトリに保存
- 差し戻しループ(`route:`)、**並列実行(`parallel:` / `foreach:`)**、**セッション再開(`continue_from:`)**、**自動要約(`compact:`)** を宣言的に書ける
- ステップごとにツール・モデル・reasoning effort・権限を自由に組み替え、`profiles:` で名前付きプリセット化

## インストール / Install

[Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest) からバイナリを落として、PATHの通った場所に置くだけ。

**Windows (PowerShell):**

```powershell
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-windows-x64.zip -OutFile sfh.zip
Expand-Archive sfh.zip -DestinationPath sfh-bin -Force ; Remove-Item sfh.zip
.\sfh-bin\sfh.exe --version
```

SmartScreenに止められたら「詳細情報 → 実行」。

**Linux (x64) / macOS (Apple Silicon):**

```bash
curl -fsSL https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-linux-x64.tar.gz | tar xz sfh
# macOS arm64: sfh-macos-arm64.tar.gz / Intel Mac: sfh-macos-x64.tar.gz / Linux arm64: sfh-linux-arm64.tar.gz
./sfh --version
```

各アセットには`.sha256`が併置してある。macOSで**ブラウザから**落とした場合は `xattr -dr com.apple.quarantine ./sfh` が必要(curlなら不要)。

**Rustがあるなら:**

```bash
cargo install --git https://github.com/Aero123421/SimpleFlowHarness
```

ソースからは `cargo build --release` → `target/release/sfh(.exe)`。

## クイックスタート

```bash
sfh init                 # 例の flow.yaml を生成
sfh validate flow.yaml   # 構文+テンプレート+ルーティングの静的チェック(タイポはここで死ぬ)
sfh run flow.yaml --var topic="Rustの非同期ランタイム比較"
```

呼び出し元エージェントからは:

```bash
sfh run research.yaml --var topic="..." -q
```

## CLI

```
sfh run <flow.yaml> [options]          フローを実行
sfh validate <flow.yaml> [--var k=v]   実行せずに検査
sfh init [file] [--force]              例のフローファイルを生成

run options:
  --var key=value     フロー変数の上書き(複数可)
  --emit <step-id>    最後にstdoutへ出すステップを指定(既定: 最後に実行されたステップ)
  --runs-dir <dir>    成果物の保存先(既定: .sfh/runs)
  --dry-run           コマンドとプロンプトをrun dirに展開して表示するだけ(実行しない)
  -v, --verbose       実行コマンドラインを表示
  -q, --quiet         進捗表示を抑制

exit code: 0=成功 / 1=フロー失敗 / 2=設定・使い方エラー
```

## フローファイル全体像

```yaml
name: research
vars:
  topic: "既定値。--var で上書き"

profiles:                    # 名前付きツール設定。ステップから use: で参照
  smart:  { tool: codex, effort: high }
  cheap:  { tool: opencode, model: opencode/some-model }
  strict: { tool: claude, access: read }

defaults:                    # 全ステップの既定値(すべて任意)
  timeout_sec: 3600
  max_visits: 3              # 同一ステップの最大実行回数(既定5)
  max_total_steps: 100
  max_parallel: 4            # parallel/foreach の同時実行数の既定
  max_prompt_chars: 80000    # レンダリング後プロンプトがこれを超えたら実行前に失敗

steps:
  - id: plan                 # 必須・一意 [A-Za-z0-9_-]
    use: smart               # プロファイル参照(ステップ直書きが優先)
    tool: codex              # codex | claude | opencode | grok | agy
    # bin: /path/to/tool     # 実行ファイル差し替え(PATHが古い時など)
    model: gpt-5-codex
    effort: high             # codex/claude/grok/agy/opencodeで意味が対応(下表)
    access: full             # read | write | full(既定: write)
    agent: plan              # opencode/claude/grok の --agent
    args: ["--foo"]          # プリセットに追加で渡す生フラグ
    cwd: path/to/dir
    timeout_sec: 1800        # 超過でプロセスツリーをkill
    max_prompt_chars: 50000
    notes: append            # このステップの出力を {{notes}} (run_dir/notes.md) に蓄積
    on_error: fail           # fail(既定) | continue | goto:<id>
    compact:                 # 出力が大きい時だけ安いモデルで自動要約して連鎖に使う
      when_over: 20000
      use: cheap
      target_chars: 2000
    prompt: |
      {{vars.topic}} について…
    route:                   # 出力に上から評価、最初にマッチした行へ。無指定なら次へ
      - when_contains: "VERDICT: REVISE"
        goto: plan
      - when_matches: "(?i)verdict:\\s*ok"
        goto: exec
      - goto: end            # end=成功終了 / fail=失敗終了 / <step-id>
```

### 並列: `parallel:`(異種メンバーのfan-out)

```yaml
  - id: ideas
    max_parallel: 3
    parallel:                 # 子は同時実行。グループの出力は "--- id ---" 区切りの集約
      - id: idea_tech
        use: smart
        prompt: "技術の観点で…"
      - id: idea_cost
        use: cheap
        prompt: "コストの観点で…"
    route:                    # 集約出力に対して評価
      - goto: report
  - id: report
    prompt: |
      {{steps.ideas.outputs}}        # 集約全文
      {{steps.idea_tech.output}}     # 子は個別にも参照できる
```

### 動的並列: `foreach:`(前段の出力の件数だけワーカー起動)

```yaml
  - id: verify
    foreach:
      from: "{{steps.hypotheses.output}}"
      split: lines            # lines | json | separator:<sep>
    max_parallel: 3
    use: cheap
    on_error: continue        # 1件失敗しても続行
    prompt: |
      仮説 {{item_index}}: {{item}} を検証せよ
```

集約は `{{steps.verify.outputs}}`。件数上限100。

### セッション再開: `continue_from:`(コンテキスト最強の節約)

```yaml
  - id: deep_dive
    tool: codex
    continue_from: plan       # planステップの会話をサーバー側コンテキストごと再開
    prompt: "さっきの計画の手順3だけ詳細化して"
```

前段の出力をプロンプトに再注入する必要がなくなる。同一ツール間のみ。5ツールすべて実機検証済み。仕組み:

| ツール | ID取得 | 再開 | 注意 |
|---|---|---|---|
| codex | stderrの`session id:`行 | `exec resume <id>` | sandboxは再開時に再指定(sfhが自動でやる) |
| claude | sfhがUUIDを`--session-id`で事前割当 | `-p -r <id>` | セッションはcwdスコープ(同じcwdで) |
| opencode | `--format json`の`sessionID` | `run -s <id>` | どのcwdからでも再開可 |
| grok | sfhがUUIDを`--session-id`で事前割当 | `--resume <id>` | cwdスコープ |
| agy | JSONの`conversation_id` | `--conversation <id>` | 不正IDは黙って新規会話→sfhがID照合して失敗検出 |

### コンテキスト管理

| 機構 | 書き方 | 効果 |
|---|---|---|
| フィルタ | `{{steps.x.output \| head:30}}` `\| tail:20` `\| truncate:4000` `\| lines:10-40` `\| trim` | 巨大出力を機械的に切る(タダ) |
| プロンプト予算 | `max_prompt_chars`(defaults/step) | 事故で巨大プロンプトに課金する前に失敗 |
| 共有ノート | `notes: append` → `{{notes}}` | 要点だけをrun_dir/notes.mdに蓄積して全文連鎖をやめる |
| 自動要約 | `compact: {when_over: N, use: <profile>}` | 閾値超過時のみ安いモデルで圧縮。`{{steps.x.output}}`は要約後、`.outputs`は原文のまま |
| セッション再開 | `continue_from:` | 再注入そのものを不要にする |

### テンプレート変数

| 変数 | 内容 |
|---|---|
| `{{vars.NAME}}` | フロー変数(未定義はエラー) |
| `{{steps.ID.output}}` | 最新出力(compact後)。未実行なら空 |
| `{{steps.ID.outputs}}` | 集約/原文(parallel・foreach・compact原文) |
| `{{steps.ID.output_file}}` | 出力ファイルパス |
| `{{item}}` `{{item_index}}` | foreach内のみ |
| `{{notes}}` | 共有ノートの現在内容 |
| `{{run_dir}}` `{{flow_dir}}` `{{step_id}}` `{{visit}}` `{{os}}` `{{prompt_file}}` | 実行環境 |

## プリセット → 実コマンド対応(2026-07-27 実機検証)

プロンプトの渡し方はツール毎に安全な経路を自動選択: **stdin**(codex/claude/opencode)、**--prompt-file**(grok ※stdin渡しはTUIが開いてハングする)、**argv**(agy ※stdin非対応、25,000字上限)。

| tool | ベース | read | write | full |
|---|---|---|---|---|
| codex | `exec --skip-git-repo-check -c approval_policy=never -o <last> -` | `-s read-only` | `-s workspace-write` | `--dangerously-bypass-approvals-and-sandbox` |
| claude | `-p --output-format text` | `--permission-mode dontAsk --tools Read,Glob,Grep,WebSearch,WebFetch,TodoWrite` | `--permission-mode acceptEdits --allowedTools Bash,WebSearch,WebFetch` | `--dangerously-skip-permissions` |
| opencode | `run --auto` ※auto必須(askで永久ハング) | `--agent plan` + edit/bash拒否env | `--agent build` + 外部dir拒否env | `--agent build`(制限なし) |
| grok | `--output-format plain --prompt-file <f>` | `--permission-mode dontAsk --deny Edit/Write/Bash` | `--permission-mode acceptEdits` | `--permission-mode bypassPermissions` |
| agy | `--print-timeout <t>s --output-format json -p <prompt>` | `--mode plan` | `--mode accept-edits` | `--dangerously-skip-permissions` |

補足:
- **Geminiモデルは `tool: agy` で使う**(gemini-3.6-flash-low 等)。旧gemini CLIには非対応(個人無料枠廃止でAntigravityに統合されたため)。
- **effortの語彙はツール毎に違う**: codex(none/minimal/low/medium/high/xhigh/max/ultra)、claude(low〜max)、grok/agy(low/medium/high)、opencodeは`--variant`(モデル毎)。範囲外はvalidateで警告。
- **opencodeのmodelは`provider/model`形式必須**。effortは`--variant`に渡る。
- **agy**: モデルIDにeffortサフィックスがある(例: gemini-3.1-pro-high)場合は`effort:`を併用しない(agyが衝突エラーを出す)。
- **claude**のネスト実行対策として`CLAUDE_CODE_SESSION_ID`等の環境変数は自動除去。
- codexの最終メッセージは`--output-last-message`ファイルから取るので思考ログは混ざらない。

## マシンローカルプロファイル

`~/.sfh/profiles.yaml`(名前→プロファイルの素朴なマップ)が自動でマージされる(フロー側が優先)。`bin:`のパスやプロバイダー選択などマシン依存の設定をフロー定義から追い出せるので、**フローYAMLはそのまま他マシンに持っていける**。

```yaml
# ~/.sfh/profiles.yaml
codex-local:
  tool: codex
  bin: 'C:\Users\you\AppData\Local\OpenAI\Codex\bin\<hash>\codex.exe'
```

## カスタムコマンド(プリセット以外)

```yaml
  - id: anything
    cmd: ["mytool", "--flag", "{{prompt_file}}"]   # 配列 = シェル介さず直接spawn(推奨)
    # cmd: "mytool < in.txt > out.txt"              # 文字列 = cmd /C | sh -c 経由
    stdin: prompt                                   # プロンプトをstdinへ流す場合
```

文字列形式の `cmd:` では、テンプレート置換値(AI出力など)に改行やシェルメタ文字(`& | < > ^ % $` 等)が含まれると**実行前にエラー**にする(シェルインジェクション防止)。その場合は配列形式か `| head:1` 等のフィルタを使うこと。

## 実行成果物(run ディレクトリ)

```
.sfh/runs/<UTC日時>-<フロー名>/
  meta.json  log.jsonl  notes.md
  <id>.prompt.txt  <id>.out.txt  <id>.err.txt  <id>.last.txt
  <id>.v2.*        (差し戻し2周目)
  <id>.i0.*        (foreachのitem 0)
  <id>.compact.*   (自動要約の中間)
```

## 既知の注意点

- **検証済みバージョン**: codex-cli 0.146.0-alpha.3.1 / claude 2.1.220 / opencode 1.18.3 / grok 0.2.112 / agy 1.0.8。エージェントCLIのフラグは変わりやすい。ズレたら `args:` で足すか `cmd:` で全部書けば逃げられる。
- **タイムアウト**: Windowsは`taskkill /T /F`、Unixはprocess group killで子孫ごと落とす。子の終了後にパイプを握り続ける孫プロセスがいてもドレイン期限で先に進む。出力は1ストリーム32MBでキャップ。
- **opencodeのread**は`OPENCODE_CONFIG_CONTENT`でedit/bashを拒否注入(1.18.3のplan agentはbashを塞がないため。実機でBLOCKED確認済み)。完全な保証が要る変更はwrite/fullレビューを挟むこと。
- **agyのexit codeは信用しない**(正常完了でexit 1がありうる)。sfhは常にJSONエンベロープの`status`で補正する。
