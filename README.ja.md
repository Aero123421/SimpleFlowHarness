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
- **プロトコルのfail-closed**: preset toolは必ず、そのCLIが文書化しているmachine-readable protocolを完了しなければなりません。エラーを表示して終了したCLIや、出力形式が変わってしまったCLIは、その文字列を回答として下流へ渡さず、stepの失敗になります。
- **機械向けインターフェース**: `run` / `plan` / `wait` / `stop` / `status` / `preflight` / `workspaces` に `--json`。JSONモードのstdoutはenvelopeのみで、失敗には分岐可能な安定コード `SFH_*` が付きます。

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
- `SFH_VERSION=1.3.0`: 特定のバージョンに固定。
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

v1.2 からは、この検証が **execution closure**（実行の閉包）全体に及びます。flow file の外側にあって実行内容を決めるもの、すなわち profile overlay、context file の中身、解決された tool version、workspace の mode と base commit、そして明示的に受け入れたリスクの一覧です。これらは run 開始時に `execution-closure.json` へ hash として固定されます。どれかが動いていれば resume は拒否され、変わった項目が名指しで示されます。

```text
SFH_EXECUTION_CLOSURE_CHANGED: the execution closure changed since this run started
  context.task: sha256:9bc86fb26f3c -> sha256:d1811c82b034
```

`--force-resume` はこれを意図的に受け入れ、`force_resume` event を記録します。後述の `--adopt-workspace` とは別の問いであり、片方がもう片方を免除することはありません。

---

## Workspace・Context・Replay（v1.2）

いずれも opt-in の3機構です。**どれも書かなければ何も変わりません**。flow は呼び出し元の作業ディレクトリで動き、`.sfh/runs` へ書き、1.1 と同じように resume します。

### managed workspace

`workspace:` は、その run の副作用がどこに属するかを宣言します。

```yaml
workspace:
  mode: auto        # current | directory | git-worktree | auto
  cleanup: auto     # auto | keep
```

`auto` は flow が宣言した `effects:` だけから決めます。prompt の内容を推測することはありません。全 step が `effects: read` なら workspace は作られません。書き得る step が1つでもあれば、run 全体で **ちょうど1個** の Git worktree を持ちます。step が何個あっても、loop が何周しても1個です。後続の step が先行 step の変更を見られるのは、同じディレクトリだからです。

worktree は、branch 元のリポジトリの外側（`--state-dir` 配下、または platform の user-state ディレクトリ）に `sfh/<flow>/<run-id>` という branch で作られます。あなた自身の checkout は変更されません。

次の2点は絶対です。

- **sfh は自分が作ったものしか削除しません。** 削除にはディレクトリ内の ownership marker と run manifest の nonce の一致が必要で、しかも削除直前に再確認します。どちらかが合わない path は warning を残して保持され、削除されることはありません。
- **未コミットの変更は自動的に破棄されません。** run の結果に関わらず dirty な workspace は残り、branch も削除されません。変更を捨てる経路は `sfh workspaces remove <run-dir> --discard` だけで、これは人間が打つコマンドです。

failed / stuck / stopped / dead で終わった run は常に workspace を保持します。そこに調査対象が残っているからです。

```bash
sfh workspaces list --state-dir ~/.local/state/sfh
sfh workspaces show <run-dir> --json
sfh workspaces clean --older-than 30 --dry-run
```

resume 時には workspace の fingerprint（HEAD、index と working tree の差分、untracked file を全件 hash、submodule 状態）を最後の durable checkpoint と比較します。未完了 step で説明できない差分は「run の外側が編集した」ことを意味するため拒否されます。`--adopt-workspace` は現在の内容を新しい基準として採用し、`workspace_adopted` event を記録します。

### `effects:` — その step が何に触るか

```yaml
- id: deploy
  effects: external      # read | workspace | external | unknown
```

sfh が推測するものではなく、利用者の宣言です。省略時は preset step では `access:` から導かれ、custom `cmd:` では `unknown`、つまり潜在的な writer として扱われます。そうでないと仮定することが、誰かの作業を失う仮定だからです。この宣言が決めるのは workspace の選択、静的 warning、replay policy だけです。

### named context

`contexts:` は、step に何を渡したかを固定します。どの source を、どの順で、どの hash で渡したかです。

```yaml
contexts:
  task:          {file: ./TASK.md}
  house_rules:   {inline: "既存設計を優先する"}
  latest_review: {template: "{{steps.review.output | optional}}"}

steps:
  - id: implement
    context: [task, house_rules, latest_review]
    context_delivery: prepend        # prepend（既定）| file
```

組み立てられた bundle は `<tag>.context.txt` に、各 source の出所・hash・サイズを記録した manifest は `<tag>.context.json` に保存されます。durable log には hash だけが載り、内容は載りません。`{{context}}` と `{{context_file}}` はどちらの delivery mode でも使えます。

context file は no-follow で読まれ、flow directory または workspace の内側に解決される必要があります。外を指す symlink は拒否されます。唯一の逃げ道は source ごとの `allow_external: true` で、使った事実は unsafe override として記録されます。`defaults.max_context_chars` を超える bundle は **何も起動する前に** 失敗します。sfh は要約もしませんし、収まるように source を落とすこともしません。何を落とすかは利用者の判断であり、template filter、`max_chars`、上流の `compact:` で表明してください。

### replay policy

step が開始されたのに終了を記録しなかった場合、resume が何をすべきか。sfh が「もう実行されたのか」を本当に知り得ない唯一のケースです。

```yaml
defaults:
  replay: {unfinished: rerun}    # rerun（既定）| stuck | fail

steps:
  - id: deploy
    effects: external
    replay: {unfinished: stuck}
```

`rerun` が既定で、これまでの release と同じ挙動です。`stuck`（exit 4）と `fail`（exit 1）は何も起動せず、workspace と部分成果物をそのまま残し、`SFH_REPLAY_REFUSED` を返します。

これは retry（同じ invocation の再試行）でも、fallback（別 profile）でも、route による再入場でも、完了済み step の結果の再利用でもありません。sfh は外部副作用について exactly-once を約束しません。約束するのは、黙って再実行しないことと、不確かな結果を成功と偽らないことです。

### `--carry-budget-from` — flow 自体が間違っていたとき（v1.3.0）

resume が答えるのは「run が中断された、続けろ」です。flow と execution closure が変わっていないことを要求しますが、それは正しい。完了済み step の再利用は、それを生んだ定義がまだ有効なときにしか意味を持たないからです。

だから resume では扱えない2つ目のケースがあります。run が止まり、証拠を読み、結論が「**flow** が間違っていた」だった場合 — 上限の値、別の binary を指していた command、絶対に発火しない route。直すのが正しい対応で、直せば closure が変わるので `--resume` は拒否します。残るのは新しい run を始めることだけで、その counter はすべてゼロから始まります。すでに使った予算は消え、辻褄を合わせる手段は flow の上限を手で書き換えることだけでした。手計算は会計ではありません。検証できず、数え間違えた瞬間に壊れ、2回目の試行が1回目の続きだったという記録もどこにも残りません。

```bash
sfh run corrected-flow.yaml --carry-budget-from .sfh/runs/20260808-021925-loop
```

先行 run の支出を持った**新しい** run を開始します:

| 引き継ぐもの | 効く上限 |
|---|---|
| leaf run 数 | `max_total_steps` |
| **step ごとの**訪問回数の最大値 | `max_visits` — 残り4周の loop は本当に残り4周 |
| 報告済み cost | `max_cost_usd` |
| active run 時間 | `wall_clock_sec` |

**counter だけです。** step output、session、routing 位置、workspace はすべて置いていきます。それらを作った flow は、これから走る flow ではないからです。`--resume` と `--carry-budget-from` は別の診断に対する別の答えなので、同時指定は usage error です。

**合成します**: 引き継いだ run からさらに引き継いでも、最初の run の支出は残ります。2回目の修正で1回目の試行が黙って消えるのは、この機能が人手から取り上げようとしているまさにその算術です。

**記録が残ります**: `budget_carried` durable event、`meta.json` の `carried_budget`、stderr の1行（`--dry-run` でも出ます）。corrected flow がもう定義していない step id は「適用できなかった」と名指しで報告し、黙って落としません。

**二重計上しません**: 引き継いだ run 自身の `cost_usd` は先行 run の支出を含みます（`max_cost_usd` はその値で判定されるので当然です）が、先行 run の行にも同じ金額が載っています。`sfh runs list` は各 run の `carried_cost_usd` を差し引いてから合計するので、修正を重ねても実際に払った額のままです。`sfh runs show` は引き継いだ分を1行で明示します。

**まだ走っている run からの引き継ぎは拒否します。** 支出が確定していないので、取った瞬間にそのスナップショットは古くなります。拒否メッセージは `sfh wait` と `sfh stop` を示します。heartbeat が止まっていても記録された process が本当にその run のものであるケース（wedged）も「まだ走っている」として扱います。

止まった run の JSON envelope は `resume` と `carry_budget` の**両方**を next action に出します。flow が悪かったのか世界が悪かったのかを知っているのは読み手だけだからです。

### `exit_conflict:` — exit code と protocol が食い違うとき（v1.2.1）

作業を終え、回答を書き、commit まで済ませたうえで、途中の tool call が 1 つ失敗していたという理由だけで非ゼロ exit を返す CLI があります。sfh の手元には turn が完了した証拠（文書化された terminal record が、壊れておらず、成功と述べている）があり、OS は失敗したと言う。正しいのは片方だけで、sfh は当てずっぽうをしません。

```yaml
steps:
  - id: implement
    tool: pi
    exit_conflict: trust_protocol   # fail（既定）| trust_protocol
```

既定は、exit status を信頼できる全 adapter で `fail` です。非ゼロ exit は従来どおり step の失敗になります。v1.2.1 で変わったのは、**sfh がこの食い違いを黙らなくなった**ことです。step の stderr、error artifact、`sfh runs why` のいずれもが「protocol は turn を成功として certify している」と述べ、この key を名指しします。

`trust_protocol` は意図的に狭く作ってあります。参照されるのは sfh が積極的な証拠を持っているとき — 認識済みの terminal record が存在し、壊れておらず、成功を報告しているとき — だけです。raw text、未知の status、壊れた envelope、terminal 欠落はこの条件を満たさないので、stdout へ出した usage error が成功 step になることはありません。使用すると `sfh plan --json` の `unsafe_overrides` に載ります。

追い詰められたときに思いつくもう一方の手 — flow から exit code 判定を消して、何であれ次段へ流す — の代わりにこちらを使ってください。あちらは fail-open で、本当に落ちた step の出力が、次にそれを読むものへ届いてしまいます。

### 再利用可能な flow: `--profiles`

共有 flow は tool ではなく役割名を書き、実行する人が中身を決められます。

```yaml
steps:
  - id: review
    use: judge          # flow には tool も model も binary も書かない
```

```bash
sfh run flow.yaml --profiles team.yaml --profiles my-machine.yaml
```

繰り返し指定でき、後のものが勝ちます。overlay は書かれた field だけを置き換えます。`args` は指定があれば置換、なければ維持。`env` は key 単位で merge。優先順位は step field > `--profiles` overlay > flow inline profile > `~/.sfh/profiles.yaml` > defaults です。step に直接 `tool:` を書く従来の書き方はそのまま有効で、overlay file は必須ではありません。

### state root

```bash
sfh run flow.yaml --state-dir ~/.local/state/sfh     # または SFH_STATE_DIR
```

`runs` / `workspaces` / `plans` / `doctor` を1つのディレクトリ配下に置きます。`--runs-dir` は従来どおり run artifacts だけを移し、どちらも指定しなければ run は今までどおり `.sfh/runs` に落ちます。state root のない managed workspace は platform の user-state ディレクトリ（`$XDG_STATE_HOME/sfh`、`$HOME/.local/state/sfh`、`%LOCALAPPDATA%\sfh`）へ fallback し、それも決められない場合はリポジトリ内へ書く代わりに error になります。

---

## プログラムから sfh を動かす

`--json` を付けると stdout は envelope だけになります。進捗と warning は stderr へ回り、設定エラーであっても prose ではなく envelope が返ります。

```bash
sfh preflight flow.yaml --json          # 無料: model 呼び出しなし
sfh plan      flow.yaml --json --save   # 何が動くか。何も起動しない
sfh run       flow.yaml --json --detach # handle と next_actions を返す
sfh wait <run-dir> --json               # 完了までブロックし、結果を返す
```

失敗には v1.2.x の全期間で意味が固定された code が付きます。message は改善され得るので、code で分岐してください。

`SFH_USAGE`, `SFH_FLOW_INVALID`, `SFH_PROTOCOL_INVALID`, `SFH_TERMINAL_MISSING`, `SFH_SESSION_UNVERIFIED`, `SFH_EXECUTION_CLOSURE_CHANGED`, `SFH_WORKSPACE_MISSING`, `SFH_WORKSPACE_DRIFT`, `SFH_WORKSPACE_BUSY`, `SFH_WORKSPACE_UNOWNED`, `SFH_REPLAY_REFUSED`, `SFH_PERSISTENCE_FAILURE`, `SFH_CAPABILITY_UNAVAILABLE`

**run directory は必ず明示してください。** path を省略したコマンドは最新の run を選び、`"implicit_target": true` を返します。agent が望む挙動であることは稀です。

`result` は `max_emit_chars` に従います。`result_file` には常に全文の path が入ります。detached run は `"terminal": false` と、答えを待つための argv を返します。

### `preflight` と `doctor`

```text
sfh preflight  — 無料。binary はあるか、version は、必要な flag は --help に残っているか。
                 protocol、session 対応、cost coverage、access の穴は。
                 各 cmd: step の program は、絶対 path でどの binary になるか。
                 この flow はどんな workspace と context を組み立てるか。
sfh doctor     — 有料。実際に 1 token の prompt を送り、sfh がまだ回答を parse できるかを確認。
                 protocol drift を捕まえられる唯一の方法。
```

`doctor` は隔離した scratch ディレクトリから実行されるため、実行した場所に置かれている instruction file ではなく adapter そのものを報告します。

v1.2.1 から、preflight は `cmd:` step が起動する program — 検証 shell、build、test runner、つまり flow が最も強く依存しているもの — も対象にします。**解決するだけで、実行はしません。** `--help` を sfh が対応している adapter へ送るのは安全でも、flow が名指しした任意の program へ送るのは安全ではないからです（`deploy.sh --help` は deploy し得ます）。解決できない名前は blocker です。Windows で bare な `bash` が `System32\bash.exe` へ解決した場合は拒否します。それは WSL launcher、つまり別の OS で、この checkout の path も worktree の `.git` file も読めないため、コードとは無関係な理由で数秒で落ちます。意図する shell を書けば（`"C:\\Program Files\\Git\\bin\\bash.exe"`）sfh は何も言いません。

sfh はどの adapter についても **minimum version を固定していません**。各 CLI の公式文書と live probe で確認していない下限を主張する代わりに、`preflight` はインストールされている version を表示し、要件は不明であると述べます。

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
- `log.jsonl`: 構造化イベントストリーム（ステップ開始・完了・トークン・コスト・protocol evidence・workspace checkpoint）
- `<step_id>.out.txt` & `<step_id>.err.txt`: サイズ制限付きのraw標準出力・標準エラー出力。32 MiBを超えるstreamは省略marker付きで先頭と末尾を保持し、構造化された最終回答とusage/costは完全なstreamから独立して処理します。
- `status.json`: リアルタイムステータススナップショット
- `execution-closure.json`: この run が固定された入力の hash
- `workspace.json`: managed workspace（flow が要求した場合）
- `<step_id>.context.txt` & `<step_id>.context.json`: 組み立てられた context とその manifest（step が context を指定した場合）

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
