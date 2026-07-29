# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**EN**: `sfh` chains AI coding CLIs — **Codex, Claude Code, opencode, Grok, Antigravity (`agy`), Pi, Cursor**, or any command — into YAML-defined multi-stage flows: review/retry loops, parallel fan-out, per-step model/effort/permission control, tool **session resume**, cost accounting with spend caps, and **crash-resume** of a whole run. It keeps your main agent's context window clean: stdout carries only the final step's output, everything else lands in a run directory. Long flows can be **detached** (`--detach`) so they outlive the caller that started them, then polled with `sfh status` and collected with `sfh wait`. Single static binary for Windows / macOS / Linux. The docs below are in Japanese, but the YAML reference tables are language-neutral — and your favorite AI can translate the rest. A JSON Schema for flow files lives in [schema/flow.schema.json](schema/flow.schema.json).

---

AI CLI(codex / claude / opencode / grok / agy / pi / cursor / 任意コマンド)を **YAML定義の多段フロー** で非対話実行する小さなオーケストレータ。単一バイナリ、実行時依存なし、Windows / macOS / Linux 対応。

メインで使っているエージェント(例: codex app)からサブエージェント群を直接管理するとコンテキストが溶けるので、その管理ループを丸ごとこのCLIに追い出すのが目的。

- **stdoutには最終ステップの出力だけ**が出る(呼び出し元エージェントはそれだけ読めばいい)
- 全ステップのプロンプト・出力・ログ・**トークン/コスト**は run ディレクトリに保存
- 差し戻しループ(`route:`)、**並列実行(`parallel:` / `foreach:`)**、**セッション再開(`continue_from:`)**、**自動要約(`compact:`)** を宣言的に書ける
- ステップごとにツール・モデル・reasoning effort・権限を自由に組み替え、`profiles:` で名前付きプリセット化
- **投げっぱなし実行(`--detach`)**: 呼び出し元エージェントが落ちても実行は生き残る。`sfh status` で生死確認、`sfh wait` で結果回収
- 無人運転前提の安全弁: **中断した実行の再開(`--resume`)**、**金額上限(`max_cost_usd`)**、リトライ/フォールバック、Ctrl+Cや強制終了でも**子AIプロセスを道連れに終了**

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

長いフローは**投げっぱなしにできる**。呼び出し元(codex app等)がタイムアウトで落ちても実行は続く:

```bash
RUN=$(sfh run research.yaml --var topic="..." --detach)   # run dirだけ返して即終了
sfh status "$RUN"                                          # 生きてる? 今どこ? いくら使った?
sfh wait   "$RUN"                                          # 終わるまで待って結果をstdoutへ
```

## CLI

```
sfh run <flow.yaml> [options]          フローを実行
sfh status [run-dir] [--json]          実行がまだ生きているか確認(既定: 最新run)
sfh wait [run-dir] [--timeout SEC]     終了まで待って結果をstdoutへ
sfh stop [run-dir]                     実行を中止(子AIごと殺す)
sfh doctor [flow.yaml]                 プリセットが実CLIとまだ噛み合っているか検査
sfh validate <flow.yaml> [--var k=v]   実行せずに検査
sfh init [file] [--force]              例のフローファイルを生成
sfh guide                              AI向けの短いフロー記法ガイド
sfh runs list|show|clean [...]         過去の実行を一覧/詳細/掃除

run options:
  --var key=value     フロー変数の上書き(複数可)
  --emit <step-id>    最後にstdoutへ出すステップを指定(既定: 最後に実行されたステップ)
  --runs-dir <dir>    成果物の保存先(既定: .sfh/runs)
  --detach            バックグラウンドで実行し、run dirだけ出して即終了。
                      呼び出したシェルやその親が死んでも実行は続く
  --resume <run-dir>  途中で落ちた実行を再開(完了済みステップは再課金しない)
  --resume-latest     同上。そのフローの最新runを自動で選ぶ
  --force-resume      フローファイルが変わっていても再開する
  --no-partial-emit   失敗時に部分結果をstdoutへ出さない
  --dry-run           コマンドとプロンプトをrun dirに展開して表示するだけ(実行しない)
  -v, --verbose       実行コマンドラインを表示
  -q, --quiet         進捗表示を抑制

status / wait / stop options:
  status [run-dir] [--runs-dir d] [--json]
  wait   [run-dir] [--runs-dir d] [--timeout SEC] [--interval SEC] [-q]
  stop   [run-dir] [--runs-dir d]
  status exit code: 0=完了 / 1=失敗・死亡・中止 / 2=判定不能 / 3=実行中 / 4=stuck
  wait はフロー自身の終了コードを返す(0/1/4。--timeout 到達時のみ 3)。
  **wait のタイムアウトは実行をキャンセルしない**(止めたいなら sfh stop)

doctor options:
  doctor [flow.yaml] [--runs-dir d] [--timeout SEC]
  各ツールに1トークンのプロンプトを投げ、sfhがまだ答えを取り出せるかを検査する。
  **実際にAPIを叩く**(だから自動実行はしない、人が打つコマンド)。
  フローを渡すとそのフローが使うツールだけを、profiles の bin:/model: 込みで
  検査し、見つからないツールもエラーにする。渡さなければ全プリセットを検査して
  未インストールのものは SKIP と報告するだけ。

runs options:
  runs list [--runs-dir d] [-n N] [--json]              終了・訪問・反復・コスト付き一覧
  runs show <run-dir> [--json]                          ステップ別の終了・訪問・反復・コスト
  runs clean [--older-than 30d] [--keep 5] [--dry-run]  古いrun dirを削除

exit code: 0=成功 / 1=フロー失敗 / 2=設定・使い方エラー / 4=stuck(人間待ち)
```

失敗しても**その時点で最後に成功したステップの出力はstdoutに出る**(`--no-partial-emit`で抑制可)。呼び出し元エージェントが「何が取れて何が残っているか」を判断できるようにするため。exit 4(stuck)でも同じ部分出力が出る。

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
  max_total_steps: 100       # 実行するリーフ総数の上限(fan-out前に判定)
  max_parallel: 4            # parallel/foreach の同時実行数の既定
  tool_max_parallel:         # ツール別の同時実行上限(レート制限対策)
    opencode: 2
  max_prompt_chars: 80000    # レンダリング後プロンプトがこれを超えたら実行前に失敗
  max_emit_chars: 200000     # stdoutに出す最大文字数(既定20万。超過分は切って保存先を案内)
  max_cost_usd: 5.0          # 報告されたコストの累計がこれを超えたら中断
  wall_clock_sec: 7200       # フロー全体の実時間上限
  retry: { max: 2, backoff_sec: 5 }   # 失敗時のリトライ(指数バックオフ)
  retry_on: transient        # transient(既定,429/5xx/切断など) | any | never
  hang_after_sec: 300        # この秒数以上無出力のままタイムアウトしたら「ハング」=一過性と分類(既定300)
  fork_warmup: auto          # fork_from時のウォームアップ auto(既定) | always | never
  env: { MY_VAR: value }     # 全子プロセスに渡す環境変数

steps:
  - id: plan                 # 必須・一意 [A-Za-z0-9_-]
    use: smart               # プロファイル参照(ステップ直書きが優先)
    tool: codex              # codex | claude | opencode | grok | agy | pi | cursor
    # bin: /path/to/tool     # 実行ファイル差し替え(PATHが古い時など)
    model: gpt-5-codex
    effort: high             # codex/claude/grok/agy/opencodeで意味が対応(下表)
    access: full             # read | write | full。AIステップは明示必須(既定値なし)、cmd:ステップは対象外
    allow_access_override: true  # 権限上書きargs:と高accessセッション再開を許可(既定は両方拒否)
    agent: plan              # opencode/claude/grok の --agent
    args: ["--foo"]          # プリセットに追加で渡す生フラグ
    unsafe_shell_template: true  # 例外実名: 文字列形式cmdでのテンプレート展開を許可(既定は禁止、配列形式が推奨)
    cwd: path/to/dir
    timeout_sec: 1800        # 超過でプロセスツリーをkill
    max_prompt_chars: 50000
    notes: append            # このステップの出力を {{notes}} (run_dir/notes.md) に蓄積
    on_error: fail           # fail(既定) | continue | goto:<id> | goto:end | goto:fail
    on_max_visits: goto:end  # 差し戻し回数を使い切った時の降格先(既定fail=フロー終了)
    retry: { max: 2 }        # このステップだけのリトライ
    hang_after_sec: 600      # このステップだけのハング判定しきい値(既定はdefaults、無指定なら300)
    fallback: [cheap2]       # リトライ後も落ちたら、このプロファイルで再挑戦(別ツールでも可)
    allow_empty: false       # 空の最終メッセージを失敗扱いにする(AIステップの既定)
    env: { FOO: bar }        # このステップの子プロセスにだけ渡す
    env_remove: [BAZ]
    compact:                 # 出力が大きい時だけ安いモデルで自動要約して連鎖に使う
      when_over: 20000
      use: cheap
      target_chars: 2000
    prompt: |
      {{vars.topic}} について…
    route:                   # 出力に上から評価、最初にマッチした行へ。無指定なら次へ
      - when_last_line_contains: "VERDICT: REVISE"   # 最終行だけ見る(推奨・誤爆しない)
        goto: plan
      - when_matches: "(?i)verdict:\\s*ok"           # 全文を正規表現
        goto: exec
      - goto: end            # end=成功終了(0) / fail=失敗終了(1) / stuck=人間待ち(4) / <step-id>
```

判定に使うテキストは**compact前**かつ**集約ヘッダ(`--- id ---`)を含まない**ので、要約やsfhのラベルで誤爆しない。`when_last_line_contains` はレビュアーが本文中で「VERDICT: REVISEと書くべきか迷った」と述べても反応しないので、差し戻し判定はこちらを推奨。

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

失敗した leaf の途中出力を `on_error: continue` で後段へ渡す場合、sfh は
`exit` / `timed_out` を含む `[sfh: ... did not complete ...]` バナーを
`{{steps.ID.output}}` / `.outputs` の先頭へ付ける。これは成果物の評価ではなく、
sfh が起動したプロセスが完了しなかったという配管上の事実である。parallel /
foreach の集約では失敗した要素のヘッダと本文だけが標識され、成功した要素は
そのまま残る。バナーもレンダリング後の文字列なので `max_prompt_chars` に算入される。

### 合議: `when_members`(N体の票を数えて分岐する)

fan-out の**メンバーごとの成否と最終行**を数える route 述語。`parallel:` /
`foreach:` を持つステップの route でだけ使える。

```yaml
  - id: review
    max_parallel: 3
    parallel:
      - { id: rev_a, use: rev, on_error: continue, prompt: "…最後の行に REVIEW-PASS か REVIEW-FAIL…" }
      - { id: rev_b, use: rev, on_error: continue, prompt: "…" }
      - { id: rev_c, use: rev, on_error: continue, prompt: "…" }
    route:
      - when_members: { last_line_is: "REVIEW-PASS", at_least: 3 }   # or all: true
        goto: wrap
      - goto: fix        # 数えられない出力・失敗・タイムアウトは全部こちら
```

1票と数える条件は**次の両方**:

1. そのメンバーが正常終了している(`exit == 0` かつ timeout でも中断でもない)。
2. そのメンバー**自身の**出力の最終非空行が `last_line_is` と**完全一致**する
   (前後の空白と CR は落とす)。

**なぜ文字列で数えてはいけないか。** グループの判定テキストは各メンバーの生出力を
連結したものだが、そこには**失敗の印が付かない**(`[sfh: FAILED]` バナーが付くのは
ラベル付き集約 `{{steps.ID.outputs}}` の側だけ)。つまり「REVIEW-PASS と言った」と
「REVIEW-PASS と言った上で exit 1 した」がテキスト上は同一である。`sh -c "grep -c …"`
での集計はこれを区別できず、さらに本文中に引用された単独行まで票に数え、そもそも
グループの route は連結全体の最終行しか見ない。`when_members` は sfh 自身が持つ
メンバー別の記録から数えるので、この3つとも起こらない。

- 量化子は `at_least: <n>`(n ≥ 1)か `all: true` の**どちらか一方が必須**。
- **母数が0なら常に不成立**。`all: true` の空集合真(foreach が0件生成した場合)は
  「全員一致」ではなく「誰も何も決めていない」であり、fail-closed 側に倒す。
- 同一規則内で他の述語(`when_contains` 等)と併用できない(validate エラー)。
  「AND で繋ぐ」意味論は作らない — 別々の規則に分けること。
- 合議イディオムではメンバーに `on_error: continue` を付けるのが正。付けないと
  1体の失敗がグループ自体の失敗になり、route が**評価されない**(on_error 経路に入る)。
- `position` イベントに `votes` / `voters` が残るので、何票入って誰が入れたのかは
  ログから読める(下記)。

**既知の限界**:

| 事柄 | 挙動 |
|---|---|
| `parallel` で `at_least` がメンバー数を超える | validate がエラーにする(構成が静的に分かるため) |
| `foreach` で `at_least` が生成件数を超える | 件数は実行時にしか分からないので validate は通る。実行時は永遠に不成立(catch-all へ) |
| 判定行が200文字を超える | 記録される最終行は200文字で切られ、比較もその形で行う(live と resume の判定を一致させるため)。リテラルの `last_line_is` が200文字超なら validate がエラーにする |

### セッション再開: `continue_from:`(コンテキスト最強の節約)

```yaml
  - id: deep_dive
    tool: codex
    continue_from: plan       # planステップの会話をサーバー側コンテキストごと再開
    prompt: "さっきの計画の手順3だけ詳細化して"
```

前段の出力をプロンプトに再注入する必要がなくなる。同一ツール間のみ。5ツールすべて実機検証済み。またsfhはセッションの**作成時accessを記録**し、それより高いaccessでの `continue_from:` / `fork_from:` は既定で**拒否**する(readで取り込んだ未信頼コンテキストをfullへ昇格させる経路を塞ぐ)。受け入れる場合のみ `allow_access_override: true`。仕組み:

| ツール | ID取得 | 再開 | 注意 |
|---|---|---|---|
| codex | stderrの`session id:`行 | `exec resume <id>` | sandboxは再開時に再指定(sfhが自動でやる) |
| claude | sfhがUUIDを`--session-id`で事前割当 | `-p -r <id>` | セッションはcwdスコープ(同じcwdで) |
| opencode | `--format json`の`sessionID` | `run -s <id>` | どのcwdからでも再開可 |
| grok | sfhがUUIDを`--session-id`で事前割当 | `--resume <id>` | cwdスコープ |
| agy | JSONの`conversation_id` | `--conversation <id>` | 不正IDは黙って新規会話→sfhがID照合して失敗検出 |
| pi | sfhがIDを`--session-id`で事前割当 | 同じ`--session-id`(作成と再開が同一フラグ) | cwdスコープ。**IDが一致しても別セッションでありうる**ため、sfhはヘッダのタイムスタンプ(マーカー)も照合する |
| cursor | sfhがIDを`--resume`で事前割当 | 同じ`--resume`(作成と再開が同一フラグ) | cwdスコープ。存在しないIDは黙って新規チャットになるため、sfhはチャット実体の存在を事前確認し、cwdが違えば拒否する |

### セッション分岐: `fork_from:`(fan-outのコンテキスト再利用)

`continue_from` は1本の会話を伸ばしますが、`fork_from` は**親の会話を枝分かれ**させます。子はそれぞれ独立したセッションを持つので、**同じ親から何本でも同時に分岐**できます(`continue_from` を兄弟で共有するのはバリデーションで禁止 — 1つのセッションに同時書き込みするため)。

```yaml
  - id: plan
    tool: claude
    prompt: "この課題の前提と制約を整理して"

  - id: fan
    parallel:
      - id: angle_a
        tool: claude
        fork_from: plan        # 親の文脈を継承しつつ独立
        prompt: "技術的リスクの観点で深掘りして"
      - id: angle_b
        tool: claude
        fork_from: plan
        prompt: "コストの観点で深掘りして"
```

対応: **claude / opencode / grok / pi**。codex(forkがTUI専用で`exec resume`は親に追記される)と agy(fork機能なし)は**バリデーションで拒否**します — 黙って冷たいセッションで走らせると、子は親の文脈を失ったまま自信のある誤答を返してexit 0になるためです。

**重要 — なぜ安くなるのか、いつ安くならないのか。** forkそれ自体はトークンを減らしません。子のプロンプト先頭が親と**バイト単位で同一**になることで、プロバイダのプロンプトキャッシュがヒットするのが実体です。したがって**N本を同時に投げると全員がキャッシュ書き込みを競合して全員ミス**します。sfhは既定で**1本目だけ先に走らせてから残りを解放**します(ウォームアップ)。実測(claude、3分岐):

| | コスト |
|---|---|
| 親(cold) | $0.0673 |
| 1本目(ウォームアップ) | $0.0340 |
| 2本目・3本目(ウォーム後) | **各 $0.0065**(5.2倍安い) |

3本同時なら約$0.102 → ウォームアップ経由で$0.047(**54%削減**)。fan-outが広く文脈が大きいほど差が開きます。この待ち時間が惜しい場合は `defaults.fork_warmup: never`、逆に全ツールで常に行うなら `always`(既定 `auto` = 実測で効果のあったclaudeのみ)。

fork失敗の検知: 4ツールとも存在しない親IDには**モデル呼び出し前にexit 1**で落ちます。加えてsfhは、子が**親のセッションIDを返してきたら失敗扱い**にします(forkフラグが無視されて親に追記された状態)。piはさらに`parentSession`で親を明示するので、それも照合します。

循環の中では `fork_from` の意味が変わる。`continue_from` で差し戻すと、却下された試行とレビューを同じセッションへ周回ごとに積み上げ、会話だけを試行前へ戻すことはできない。`fork_from` なら各周回が同じ親セッションから新しい子を作るため、**毎回「試行前の会話状態」からやり直せる**。ワークツリーまで巻き戻す機能ではないが、差し戻しループの実装セッションにはこちらが合う。具体例は後述のイディオム集に示す。

### コンテキスト管理

| 機構 | 書き方 | 効果 |
|---|---|---|
| フィルタ | `{{steps.x.output \| head:30}}` `\| tail:20` `\| truncate:4000` `\| lines:10-40` `\| trim` | 巨大出力を機械的に切る(タダ) |
| プロンプト予算 | `max_prompt_chars`(defaults/step) | 事故で巨大プロンプトに課金する前に失敗 |
| 共有ノート | `notes: append` → `{{notes \| tail:120}}` | chain output全文をnotes.mdへ追記する。参照側で直近N行に制限する |
| 自動要約 | `compact: {when_over: N, use: <profile>}` | 閾値超過時のみ安いモデルで圧縮。`{{steps.x.output}}`は要約後、`.outputs`は原文のまま |
| セッション再開 | `continue_from:` | 再注入そのものを不要にする |
| stdout上限 | `max_emit_chars` | 呼び出し元のコンテキストを機械的に守る |

`compact:` は**sfhが唯一AIに指示を書く場所**なので、既定の指示文をここに明記しておく(`instruction:` で差し替え可能):

> Summarize the text below in at most {target} characters, in the same language as the text. It will be passed to another AI agent as context, so keep every conclusion, number, file path and open question. Output only the summary.

要約器が失敗した場合はsfhが**先頭+末尾を機械的に残す**(head+tail)。原文は `<id>.precompact.txt` に保存され、`--resume` してもそれが `{{steps.x.outputs}}` に復元される。

### コストとトークンの会計

全プリセットを機械可読モード(`--output-format json` 等)で起動しているので、**各ステップのトークン数と(報告される場合は)USDコストが自動で記録**される。

- 進捗表示に `$0.0661` のように出る / `log.jsonl` の各 `step_end` に `input_tokens` `output_tokens` `cost_usd`
- `sfh runs list --json` / `sfh runs show <dir> --json` で後から機械集計できる。`visit` は最大訪問番号、`repeat` は同じステップで同一 `output_hash` が連続した時の初回を除く最大反復回数
- `runs list` の末尾(`--json` では `total_cost_usd`)は、`-n` 適用後に選ばれたrun群の報告済みコスト合計
- `defaults.max_cost_usd` を超えたら**次のステップを始める前に中断**(無人運転の金額ガード)
- コスト報告があるのは claude / grok / opencode。codex と agy はトークン数のみ

### 失敗からの回復

```bash
sfh run research.yaml --resume-latest        # 落ちた所から再開(完了済みは再課金なし)
sfh run research.yaml --resume .sfh/runs/20260727-120000-research
```

`log.jsonl` から完了済みステップの出力・訪問回数・セッションID・累計コストを復元し、失敗したステップから再開する。フローファイルが変更されていると指紋(SHA-256)不一致で拒否する(`--force-resume`で強行)。

成功した `step_end` の直後、次の `position` を記録する前に sfh が停止していた場合は、
そのステップを再実行せず、保存済み chain 出力に対して route 規則だけを再評価し、
決定を `position` イベントとして追記する。一方、`step_start` はあるが対応する
`step_end` が無いコマンドは完了したか判定できないため再実行する。resume 前に
コマンドライン付きで警告し、同じ情報を `status.json.unfinished_step` にも残す。

- **リトライ**: `retry: {max: 2, backoff_sec: 5}` — 既定では 429 / 5xx / 接続断など**一過性と判定できる失敗のみ**再試行(指数バックオフ)。`retry_on: any` で何でも、`never` で無効
- **ハングは一過性に数える**: タイムアウトは従来まとめて非一過性だった。`hang_after_sec`(既定300)以上**何も出力しないまま**タイムアウトした試行だけは「時間切れ」ではなく「パイプが死んだ」と分類し、`retry_on: transient` の再試行対象にする。出力を出し続けたまま時間切れになった試行は従来どおり再試行しない(同じ予算を二度焼くだけだから)
- **フォールバック**: `fallback: [profile_a, profile_b]` — リトライ後も落ちたら別プロファイル(別プロバイダ・別モデルでも可)で再挑戦
- **差し戻しループの降格**: `on_max_visits: goto:summarize` — 3回REVISEされたら諦めて要約に進む、が書ける(既定はフロー失敗)

### 第3の終端: `goto: stuck`(exit 4 = 作業は残っているが人間待ち)

「**作業は保存されているが、人間の判断なしに先へ進んではいけない**」を機械可読にする終端。`end`(exit 0)/`fail`(exit 1)と同格の予約 goto 先で、`route[].goto` / `on_error: goto:stuck` / `on_max_visits: goto:stuck` のどこでも書ける。

```yaml
  - id: verdict
    prompt: "…最終行に RESOLVED か UNRESOLVED だけを書け"
    route:
      - when_last_line_is: "RESOLVED"
        goto: wrap
      - goto: stuck          # 収束しなかった。作業は run dir に残っている
  - id: fixer
    max_visits: 3
    on_max_visits: goto:stuck    # 3周しても直らないなら人間に返す
```

これまでこの状況は `goto: end`(=成功と同じ顔)か、「最終行 UNRESOLVED を呼び出し元が grep する」という文字列規約で書くしかなかった。前者は非収束を成功と誤報し、後者は sfh が v1 で潰してきた fail-open と同型なので、終端そのものを増やしてある。

**sfh の判断は一切増えない。** ユーザーが `goto: stuck` と宣言した所に到達したときだけ起きる。

| 見え方 | 値 |
|---|---|
| `sfh run` の exit code | **4** |
| `status.json` | `"state": "stuck"`, `"exit_code": 4`, `"error": "routed to stuck after '<step>'"` |
| `sfh status` / `sfh wait` の exit code | **4**(`sfh wait` は部分出力も stdout に出す) |
| `runs list` / `runs show` の STATUS | `stuck` |
| stdout | 失敗時と同じ部分出力(`--no-partial-emit` で抑制可) |
| `log.jsonl` | 通常の `position` イベント(`"next":"stuck"`、`via` は rule / catch_all / on_error / max_visits) |

偽装された `status.json` の `state: "stuck"` は `failed` と同じ nonce 検査を通らないと報告されない(新しい状態名だからといって信用度は上がらない)。

**再開できる。** stuck した run は `completed` 扱いにしないので、そのまま `--resume` できる:

- **route 経由**: 再開開始点は stuck へ分岐した**そのステップ**で、**再実行**される(visit +1、通常どおり `max_visits` 検査に服す)。記録済みの判定テキストを再生する道は取らない — 同じテキストを再評価すれば必ず同じ stuck に戻るだけだから。人間が「何に詰まっていたか」を直してから再開すれば、今度は別の枝へ進む。
- **on_max_visits 経由**: 再開すると入場時の visit 検査に再び引っかかり、**即座にまた stuck になる**。これは仕様。visit カウンタを黙ってリセットするほうが嘘になる。正しい道はフローの `max_visits` を直して `--force-resume` すること。

> **破壊的変更**: ステップ id `stuck`(**大文字小文字を無視**して比較。id の重複検査と同じ規則)は予約語になり、`sfh validate` が明示エラーで拒否する。既存フローに `stuck` という id があれば改名が必要 — ただし黙って挙動が変わるのではなく、validate が大声で落ちる。

### 投げっぱなし実行: `--detach`(親の寿命から切り離す)

呼び出し元のAIエージェントは、シェルツールのタイムアウトやセッション終了で数十分後には落ちる。フォアグラウンドで待たせているとそこで巻き添えになるので、`--detach` で**親のジョブオブジェクト(Windows)/セッション(Unix)の外**へ実行を追い出せる:

```bash
RUN=$(sfh run research.yaml --var topic="..." --detach)
# → stdoutにはrun dirのパスだけ。呼び出し元は即座に解放される
```

以降は好きなタイミングで問い合わせる:

```bash
sfh status "$RUN"          # running / done / failed / stuck / dead / stopped
sfh status "$RUN" --json   # 同じ内容を機械可読で(親エージェント向け)
sfh wait   "$RUN"          # 終了まで待ち、フォアグラウンド実行と同じ結果をstdoutへ
sfh stop   "$RUN"          # 中止。起動済みの子AIごと殺す
```

**`dead` が肝心**。`status.json` が `running` のまま残っていても、記録されたpidが消えていれば(=親ごと殺された)、sfhはそれを `running` ではなく `dead` と報告し、再開コマンドを出す:

```
dead     3 steps, $0.1240 - process 64012 is gone
sfh: this run was killed before it finished. resume with: sfh run research.yaml --resume .sfh/runs/...
```

pidの生存確認と**ハートビートの鮮度**の両方を見る(pidは再利用されるため片方だけでは信用できない)。

> Windowsでは、呼び出し元プロセスがブレイクアウェイを許可しないjob objectを張っていると切り離しに失敗する。その場合sfhは黙って諦めず、**「起動はしたが親と心中する可能性がある」と警告を出して実行する**。

### 実行中の監視(無人運転)

run dir の `status.json` が3秒ごとに更新される。`sfh status` を使わず直接読んでもいい:

```json
{ "state": "running", "current_step": "execute", "heartbeat_utc": "20260727-135338",
  "step_started_utc": "20260727-131240", "last_output_utc": "20260727-131512", "visit": 2,
  "steps_done": 5, "cost_usd": 0.0974, "pid": 64012 }
```

終了時には `exit_code` / `emit_step` / `emit_file` / `error` が追記される(`sfh wait` はこれを見て結果を返す)。

### 二つの時計 — 「経過時間」と「最終出力からの時間」

`state: running` と生きているpidとハートビートは、**112分間一言も出力しないまま固まっていた実行**を全部「正常稼働中」と報告した。経過時間だけでは、40分かかるステップが働いているのか、38分前に黙って死んだのかが区別できない。そこで観測している側の事実、つまり**子プロセスの出力が最後に届いた時刻**を記録して出す:

```
running  3 steps, $0.3100 - fix (visit 2), 41m elapsed, 38m since last output, 2s since heartbeat
```

- `status.json`: `step_started_utc`(現ステップの開始)/ `last_output_utc`(全子プロセス横断の最終出力。まだ誰も何も出していなければ `null`)/ `visit`(現ステップの周回数)。`sfh status --json` にも同じ3キーが出る
- `log.jsonl` の `step_end`: `idle_ms` — 終了(またはkill)時点で何ミリ秒黙っていたか。一度も出力しなければ実行時間そのもの
- 計測は **stdout と stderr の両方**。進捗をstderrにしか出さないCLIがあるため、片方だけを見るとその全タイムアウトがハング扱いになる
- 打ち切りはしない。sfhはこの時計で**プロセスを殺さない**(分類と露出だけ)。停滞で止めたい場合は、この値をポーリングして呼び出し元が `sfh stop` を打つ

**既知の退化**: sfhが観測できるのはパイプの活動であって、モデルの活動ではない。最後にJSONを一塊で吐くプリセットでは idle ≒ 経過時間になり、ハング分類は「タイムアウトなら常に1回リトライ」へ退化する。

| プリセット | stdoutの出方(sfhの解析形式) | idle時計 |
|---|---|---|
| codex / opencode / pi | イベントを逐次(JSONL / NDJSON) | 実効。無言ハングと純粋な時間超過を区別できる |
| claude / grok / agy | 最後にJSONを一塊 | **退化**。idle ≒ 経過時間 → タイムアウトは常に1回リトライ |
| cursor | 最後にJSONを一塊(experimental) | 同上 |
| `cmd:` | コマンド次第 | コマンド次第。逐次出力するコマンドなら実効 |

退化した側でも、ゼロ出力で死んだ試行を1回だけ再試行する損失はゼロなので、この退化は許容している。区別が本当に要るステップは、進捗を吐く `cmd:` で包むか `hang_after_sec` をタイムアウトより長くして分類自体を切ること。

Ctrl+C・親プロセスの死・強制終了のいずれでも、**起動済みのAI CLIプロセスは道連れで終了する**(Windowsはjob object、Linuxは`PR_SET_PDEATHSIG`+プロセスグループkill)。放置されたエージェントが課金し続ける事故を防ぐ。`--detach` で起動した実行だけがこの規則の例外で、これは明示的に要求された場合に限られる。

### テンプレート変数

| 変数 | 内容 |
|---|---|
| `{{vars.NAME}}` | フロー変数(未定義はエラー) |
| `{{steps.ID.output}}` | 最新出力(compact後)。未実行なら空 |
| `{{steps.ID.outputs}}` | 集約/原文(parallel・foreach・compact原文) |
| `{{steps.ID.output_file}}` | 出力ファイルパス |
| `{{steps.ID.exit}}` | sfhが正規化した終了コード。プロセス終了コードそのものではなく、出力解析・空出力・セッション検証等の結果も反映する。診断用テンプレート値であり `route:` の述語ではない |
| `{{steps.ID.stderr_file}}` | 標準エラー出力ファイルのパス |
| `{{item}}` `{{item_index}}` | foreach内のみ |
| `{{notes}}` | 共有ノートの現在内容 |
| `{{run_dir}}` `{{flow_dir}}` `{{step_id}}` `{{visit}}` `{{os}}` `{{prompt_file}}` | 実行環境 |
| `{{raw}}...{{endraw}}` | 中身をそのまま出す。**テンプレートの話をするプロンプト**(「このHandlebarsを直して: `{{user.name}}`」)はこれで囲む。囲まないと未定義キーとして実行前にエラーになる |

## プリセット → 実コマンド対応(2026-07-27 実機検証)

プロンプトの渡し方はツール毎に安全な経路を自動選択: **stdin**(codex/claude/opencode/pi/cursor)、**--prompt-file**(grok ※stdin渡しはTUIが開いてハングする)、**argv**(agy ※stdin非対応、25,000字上限)。

| tool | ベース | read | write | full |
|---|---|---|---|---|
| codex | `exec --skip-git-repo-check -c approval_policy=never -o <last> -` | `-s read-only` | `-s workspace-write` | `--dangerously-bypass-approvals-and-sandbox` |
| claude | `-p --output-format json` | `--permission-mode dontAsk --tools Read,Glob,Grep,WebSearch,WebFetch,TodoWrite` | `--permission-mode acceptEdits --allowedTools WebSearch,WebFetch` | `--dangerously-skip-permissions` |
| opencode | `run --auto` ※auto必須(askで永久ハング) | `--agent plan` + edit/bash拒否env | `--agent build` + bash/外部dir拒否env | `--agent build`(制限なし) |
| grok | `--output-format json --prompt-file <f>` | `--permission-mode dontAsk --deny Edit/Write/Bash` | `--permission-mode acceptEdits` | `--permission-mode bypassPermissions` |
| agy | `--print-timeout <t>s --output-format json -p <prompt>` | `--mode plan` | `--mode accept-edits` | `--dangerously-skip-permissions` |
| pi | `--mode json --offline` | `--tools read,grep,find,ls` +拡張/スキル無効化 | `--tools read,edit,write,grep,find,ls` +拡張/スキル無効化 | `--tools read,bash,edit,write,grep,find,ls --approve` |
| cursor | `-p --output-format json --trust --disable-auto-update --disable-project-configs` | `--mode plan`(全承認要求を拒否) | **(validateエラー)** | `--force` |

補足:
- **Geminiモデルは `tool: agy` で使う**(gemini-3.6-flash-low 等)。旧gemini CLIには非対応(個人無料枠廃止でAntigravityに統合されたため)。
- **effortの語彙はツール毎に違う**: codex(none/minimal/low/medium/high/xhigh/max/ultra)、claude(low〜max)、grok/agy(low/medium/high)、pi(off〜max、`--thinking`)、opencodeは`--variant`(モデル毎)。範囲外はvalidateで警告。
- **pi(`@earendil-works/pi-coding-agent`)にはサンドボックスも権限プロンプトも存在しない**(設計思想として意図的)。素の`pi`は既にread+bash+edit+writeが有効なので、sfhは`--tools`の許可リストで権限を表現する。`read`はツール登録レベルで確実だが、`write`は**書き込み先を制限しない**(ワークスペース境界がない)。またread/writeではシェルを登録せず、拡張・スキル・プロンプトテンプレートも無効化する — リポジトリ内に置かれた拡張がfull process rightsでBashを登録すれば許可リストが無意味になるから。コマンド実行が必要なら`access: full`と明示する(または`args: ["-t", "..."]` + `allow_access_override: true`)。
- **piの`agent:`は無効**(`--agent`が無い)。ペルソナは`args: ["--append-system-prompt", "..."]`で。
- **cursor(`cursor-agent`)の権限は非対話モードでは2段階しかない**: `--force`なしは全拒否、ありは全承認(シェル含む)。中間段階は存在しないため、**`access: write`はvalidateエラー**。cursorを使うステップは`read`か`full`を明示してください(既定値がないため、accessを書かないステップも通りません)。`effort:`と`agent:`も非対応(effortはモデルID側に`-thinking`等で埋め込む)。
- **cursorの`continue_from`は再開先の実在をsfhが検証してから実行します。** cursorは`--resume`が「作成も再開も兼ねる」ため、存在しないIDを渡すと**黙って新規チャットを作り、そのIDをそのまま返します**。そこでsfhはチャット実体(`~/.cursor/chats/<cwdハッシュ>/<id>/store.db`)のパスを記録し、再開前に存在を確認します。加えてcursorのセッションは**cwd単位**なので、作成時と違うディレクトリからの再開は警告ではなく**エラーで拒否**します(そのまま走らせると文脈ゼロの別チャットになるため)。実測では再開時の入力トークンが 19,817 → 876 まで下がり、非常に効率的です。
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

## sfh が判断する境界

> **sfh は自分の配管については判断する。仕事については絶対に判断しない。**

「能力を足さず、順番と分岐だけ」という説明はもう正確ではない。`compact:` はsfhが選んだモデルと指示で下流の文脈を書き換え、`retry_on: transient` は既知の一過性エラー表現を照合して再試行を決める。`hang_after_sec` の無出力判定も同種で、パイプが黙った時間を見て再試行の可否を決める。いずれも配管を維持するための判断である。一方、成果物が正しいか、レビューに合格したか、作業が停滞したかはsfhには決めさせない。その判定はユーザーが `cmd:` と `route:` で明示し、sfhは終了コード・出力・訪問・コストという観測事実だけを記録する。

## カスタムコマンド(エスケープハッチ)

```yaml
  - id: anything
    cmd: ["mytool", "--flag", "{{prompt_file}}"]   # 配列 = シェル介さず直接spawn(推奨)
    # cmd: "mytool < in.txt > out.txt"              # 文字列 = cmd /C | sh -c 経由(テンプレート展開は既定で禁止)
    stdin: prompt                                   # プロンプトをstdinへ流す場合
```

**文字列形式の `cmd:` でのテンプレート展開は既定で禁止**(validateエラー)。置換値はシェル文字列へ注入されるためで、メタ文字のブラックリストは安全境界にならない — 監査の実例では、上流AI出力 `--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt` が禁止文字を1つも含まないまま `tar` の危険オプションとして実行される。本当に必要なステップだけ `unsafe_shell_template: true` を明示すること(置換値のメタ文字チェックは残るが、それは区切り文字のフィルタに過ぎない)。エラー文が案内するとおり、**配列形式 `cmd: [...]` に移行するのが正解**。

配列形式ではシェルを介さず、置換値は1つの引数としてそのまま渡る(シェルインジェクションは起きない)。ただし「引数として渡る」こと自体は変わらない: 上記の値は `tar` の引数になれば依然として危険オプションでありうるので、対象プログラムのオプション解釈に対する保証ではない。

## イディオム集

以下はsfhに仕事の判定能力を足さず、ユーザー所有の判定器を配管へ組み込むための形である。

### fail-closed ゲート

通過条件だけを肯定的に書き、最後の述語なしルールを差し戻し側にする。判定不能な出力を通さない。

```yaml
defaults:
  max_visits: 3
steps:
  - id: implement
    tool: claude
    on_max_visits: goto:manual_review
    prompt: |
      修正してください。前回の指摘:
      {{steps.review.output | tail:40}}
  - id: review
    tool: claude
    prompt: |
      {{steps.implement.output}}
      合格なら最終行を VERDICT: OK にしてください。
    route:
      - when_last_line_contains: "VERDICT: OK"
        goto: accepted
      - goto: implement                 # 読めない出力も差し戻す
  - id: accepted
    cmd: ["echo", "accepted"]
    route: [{goto: end}]
  - id: manual_review
    cmd: ["echo", "visit limit reached"]
```

`max_visits` は実行後ではなく**ステップ入場時**に検査する。`implement → review → implement` で両者の上限が同じなら、毎周先に入る `implement` が先に上限を超えるため、`on_max_visits` もそこへ置く。

### 終了コードでルーティングする決定論的検証器

検証器は配列形式の `cmd:` で直接起動し、失敗時だけ `on_error:` で修正役へ送る。修正役は保存済みstderrを `{{steps.test.stderr_file}}` から読むため、シェルを挟まずWindows / macOS / Linuxで同じになる。

```yaml
  - id: test
    cmd: ["cargo", "test"]
    on_error: goto:fix
  - id: accepted
    cmd: ["cargo", "--version"]
    route: [{goto: end}]
  - id: fix
    tool: claude
    prompt: "{{steps.test.stderr_file}} を読んでテストを直してください"
    route: [{goto: test}]
```

`sh -c` は複数コマンドをどうしても連結するときだけ使い、Windowsでは別途Git Bashが必要になる。

**`sh -c` の中にテンプレートを書いてはいけない。** スクリプト文字列はシェルに再解釈されるので、`{{...}}` を直接埋め込むと拒否される。値は**スクリプトの後ろの引数**として渡すこと。`$1`, `$2` … として届き、シェルに再パースされない。

```yaml
    # 拒否される: 値がシェルに再解釈される
    cmd: ["sh", "-c", "grep {{steps.a.output}} file"]

    # 正しい: $1 として届く。スクリプト側で "$1" と引用すること
    cmd: ["sh", "-c", "grep -- \"$1\" file", "search", "{{steps.a.output}}"]
```

`cmd /c` と `powershell -Command` は後続引数を1本のコマンド行に連結し直すため、この手は使えない。そちらは配列形式(`["program", "--flag", "{{...}}"]`)にする。

`EXIT=$?` を最終行へ出す手法は、`stderr_file` が無かった時代の回避策としてのみ残る。

### 改竄トリップワイヤ

追跡済みファイルはGitのindexを基準に、宣言したパスだけを比較する。

```yaml
  - id: tests_tripwire
    cmd: ["git", "diff", "--quiet", "--exit-code", "--", "tests"]
    allow_empty: true
    on_error: goto:tampered
```

未追跡ファイルも拒否するPOSIX版:

```yaml
  - id: untracked_tests_tripwire
    cmd: ["sh", "-c", "test -z \"$(git ls-files --others --exclude-standard -- tests)\""]
    allow_empty: true
    on_error: goto:tampered
```

これは宣言パスだけの内容比較で、Gitのindexキャッシュが効き、そのまま `--dry-run` に現れてコピペできる。sfh固有の `protect:` キーにはしない。暗黙機能は `--dry-run` のコマンドとして印字できず、表示結果をコピーしたフローからトリップワイヤだけが消え、完全性の約束が壊れるためである。なお最初の形式はworking tree対indexなので、基準をHEADにしたい場合は `diff` の直後に `HEAD` を足す。

### 人間ゲート

sfhは子プロセスのstdinを端末へ直結しない。次はrun固有の回答ファイルを待つ、実際にブロックするPOSIX版である。

```yaml
  - id: package
    cmd: ["echo", "release-candidate.zip"]
  - id: approve
    cmd: ["sh", "-c", "cat >&2; while [ ! -s \"$1\" ]; do sleep 1; done; cat \"$1\"", "human-gate", "{{run_dir}}/approval.txt"]
    stdin: prompt
    prompt: |
      release を承認するなら approval.txt に判断理由を書いてください。
      対象: {{steps.package.output}}
    timeout_sec: 3600
    on_error: goto:approval_expired
    route: [{goto: approved}]
  - id: approved
    cmd: ["echo", "approved: {{steps.approve.output | trim}}"]
    route: [{goto: end}]
  - id: approval_expired
    cmd: ["echo", "approval expired"]
```

表示内容は `stdin: prompt` でコマンドへ渡り、`<run-dir>/approve.err.txt` に残る。人間は確認後に `<run-dir>/approval.txt` を作る。期限は `timeout_sec`、期限切れの方針は `on_error`、回答ファイルの内容は `approve` のchain outputとして後段へ渡る。GUI・チケット・チャット承認を使う場合も、同じstdin/stdout契約のコマンドへ差し替えればよい。

期限切れ側を `on_error: goto:stuck` にすると、「承認されないまま終わった」を exit 4 で呼び出し元に申告できる(成功でも失敗でもない、という事実そのものを返す)。

### ケース行列

ケースごとに独立したrun dirを固定し、失敗ケースがあっても全件を回す。

```yaml
vars:
  cases: |
    parser-empty
    parser-large
    parser-invalid
steps:
  - id: matrix
    foreach:
      from: "{{vars.cases}}"
      split: lines
    max_parallel: 3
    cmd: ["sfh", "run", "case.yaml", "--var", "case={{item}}", "--run-dir", "evruns/{{item}}", "-q"]
    on_error: continue
```

`case.yaml` 内の外部採点器が返す終了コードをground truthにする。sfhはその意味を推測せず記録するだけで、集計は次の一行で得られる。

```bash
sfh runs list --runs-dir evruns --json
```

専用の `sfh eval` は要らない。必要な分類はユーザーの採点器と `jq` で計算する。

### ユーザーが所有する停滞検知

```yaml
  - id: artifact_delta
    cmd: ["git", "diff", "--stat", "--", "."]
    allow_empty: true
```

どの差分を進捗と呼ぶかは、後段のユーザー所有コマンドで決める。sfhが `output_hash` の反復だけを見て自動停止してはいけない。chain outputはエージェントの最終メッセージであって成果物ではなく、同じ「完了しました」が続いてもワークツリーは進んでいる場合があるからである。

`status.json` の `last_output_utc`(上の「二つの時計」)はこれとは別の話で、**パイプが黙っている**ことしか意味しない。喋り続けながら何も作っていないエージェントは、そちらでは検知できない。

### 差し戻しループ内の `fork_from`

```yaml
  - id: baseline
    tool: claude
    prompt: "要件と現状を読み、修正前の前提を整理して"
  - id: attempt
    tool: claude
    fork_from: baseline
    max_visits: 3
    on_max_visits: goto:manual_review
    prompt: |
      修正を1案実施してください。前回の却下理由:
      {{steps.review.output | tail:40}}
  - id: review
    tool: claude
    prompt: "{{steps.attempt.output}}\n最終行は VERDICT: OK または VERDICT: REVISE"
    route:
      - when_last_line_contains: "VERDICT: OK"
        goto: accepted
      - goto: attempt
  - id: accepted
    cmd: ["echo", "accepted"]
    route: [{goto: end}]
  - id: manual_review
    cmd: ["echo", "visit limit reached"]
```

`continue_from: attempt` で周回すると却下済み試行が1セッションへ蓄積し続ける。`fork_from: baseline` なら各visitが同じ修正前の会話から独立分岐する。対応ツールはclaude / opencode / grok / pi。ファイル変更は共有ワークツリーに残るので、毎周ファイルまで戻したい場合はユーザー所有の `cmd:` を別途置く。

### 増え続けるnotesを末尾だけ読む

```yaml
  - id: next
    tool: claude
    prompt: |
      直近の作業記録だけを踏まえて次へ進んでください:
      {{notes | tail:120}}
```

`notes: append` は要点ではなく各ステップのchain output**全体**を `notes.md` へ追記し、ファイル自体には上限がない。裸の `{{notes}}` を毎回再注入すると、下流の `max_prompt_chars` に達したステップで初めて一発失敗する。その時点までの上流呼び出しは課金済みである。`tail:N` は末尾N行だけを機械的に渡す無料の歯止めで、必要なら `compact:` と併用する。

## 実行成果物(run ディレクトリ)

```
.sfh/runs/<UTC日時>-<フロー名>/
  meta.json        実行時の変数・sfhバージョン・各CLIの実バイナリとバージョン・合計コスト
  log.jsonl        ステップ毎のexit/所要時間/トークン/コスト/セッションID/コマンドライン
                   分岐の理由(どの規則がどの行で発火してどこへ跳んだか)も残る
  status.json      3秒ごとに更新される生存信号(state/current_step/cost_usd/pid)
                   step_started_utc / last_output_utc / visit で停滞が分かる
                   終了時に exit_code / emit_step / emit_file / error が入る
                   resume時の再実行リスクは unfinished_step に入る
  detached.*.txt   --detach 実行のstdout/stderr(sfh wait はここを返す)
  notes.md         notes: append の蓄積
  <id>.prompt.txt  レンダリング済みプロンプト
  <id>.out.txt     生stdout(実行中は逐次書かれ、終了時にANSI除去済みで確定)
  <id>.chain.txt   次段に渡った最終メッセージ(resumeはこれを読む)
  <id>.err.txt     stderr
  <id>.precompact.txt  compact前の原文
  <id>.v2.*        差し戻し2周目 / <id>.i0.* foreachのitem 0 / <id>.compact.* 自動要約
```

平 leaf の `<id>.chain.txt` は失敗時もバナー無しの生テキストで、resume は
`step_end` の `exit` / `timed_out` からメモリ上のバナーを再構成する。そのため
resume を重ねても二重バナーにならない。parallel / foreach は集約済みの一つの
文字列を `.out.txt` / `.chain.txt` / テンプレート値へ共通して書くため、集約ファイル
自体がバナー付きであり、バナー無しの集約コピーは存在しない。

### `log.jsonl` から「なぜそこへ跳んだか」を読む

`position` イベントは分岐の**理由**を持つ。`via` が `rule`(述語が一致)/ `catch_all`
(述語無しの規則)のときは、さらに次の 2 キーが付く:

| キー | 内容 |
|---|---|
| `rule` | 一致した規則の `route:` 内での 0 始まり番号 |
| `route_line` | その規則が実際に照合したテキスト。最終行系述語(`when_last_line_*`)と catch-all は**最終非空行**、全文系(`when_contains` / `when_matches`)は**判定テキストの先頭**。いずれも 200 文字で機械的に切る |

`via` が `fallthrough` / `on_error` / `max_visits` の position は規則を見ていないので、
この 2 キーは付かない(「どの規則か」を騙らないため)。

`when_members` の規則が一致した position には、さらに `votes`(票数)と
`voters`(投票したメンバーの id 配列)が付く。

`aggregate_end`(parallel / foreach 共通)には `members` が入る:

```json
"members": [{"id": "rev_a", "ok": true, "exit": 0, "last_line": "REVIEW-PASS"}, …]
```

**この記録が resume 時の唯一の情報源である。** 集約テキストは run ディレクトリに
残るが、どのメンバーが完走したかはテキストからは分からない(前述のとおり失敗の印が
付かない)。したがって、v1.1 より前に作られた run にフローを編集して `when_members` を
足し `--force-resume` した場合、sfh は**黙って catch-all に落とさずエラーで止まる**
(`this run predates per-member route records; re-run the group step or remove when_members`)。
run の世代によって分岐が静かに変わるくらいなら止まるほうがよい、という判断。

`step_end` には `os`(`windows` / `linux` / `macos`)が入る。ログは書いた機械と別の機械で
読まれるのが普通なので、「こっちでは通る」の一次資料をログ自身に持たせる。

`step_start` には `session_parent` が入る。`continue_from` / `fork_from` が解決できたときは
`{"mode":"continue"|"fork","step":"<接続先ステップ>","tool":"<CLI>","id":"<親のセッションID>"}`、
自前の文脈で始まったステップは `null`。フローを編集して `--force-resume` した後は
フロー側を読んでも実際の親子関係が分からないため、ログ側に残す。

```bash
# どのステップがどの行でどこへ跳んだか
grep '"event":"position"' log.jsonl | jq -r '[.after,.via,(.rule|tostring),.next,.route_line]|@tsv'
```

`sfh runs list` で一覧、`sfh runs show <dir>` でステップ別の明細、`sfh runs clean --older-than 30d --keep 5` で掃除。

**長いステップの様子を見たいときは `<id>.out.txt` を tail すればいい。** 子プロセスの出力は終了を待たずに逐次書き込まれるので、30分かかるステップが「進んでいる」のか「固まっている」のかが分かる。

> **プロンプトと出力は平文で残る。** 秘匿情報を扱うフローでは `--runs-dir` を安全な場所に向けるか、`sfh runs clean` を定期実行すること。Unixではsfh自身の成果物をumaskによらず所有者限定(ディレクトリ0700 / ファイル0600)に強制する。Windowsでは継承ACLに依存する(ファイル毎のDACL明示はコストに見合わないため設定していない。ユーザープロファイル配下の既定ACLは通常、所有者とSYSTEM・Administratorsのみ)。
> **runsディレクトリを作るとき、sfhはそこに `.gitignore`(中身は `*`)を自動で置く**(cargoが`target/`にやるのと同じ)。既存の `.gitignore` がある場合は中身を検証し、実効的な `*` パターンがなければ(空ファイルを先に置かれる攻撃対策)`*` を追記して警告する。ただし守られるのは通常の `git add` までで、既にコミット済みのファイルや `git add -f` はsfhの管轄外。

## 安全性について正直な話

- **`access:` は絶対的なサンドボックスではない。** 各CLIの権限フラグへの変換であり、OS境界の保証ではない。ただし権限契約を裏切る`args:`(piの`-t`/`--approve`/`--tools`、claudeの`--tools`/`--allowedTools`/`--permission-mode`、opencodeの`--agent`、grokの`--allow`/`--permission-mode`、agyの`--mode`、codexの`-s`/`--sandbox`/`-c sandbox_mode=...`、および`--force`等の全局バイパス)は、accessがfullでないステップでは**validateエラー**(fail-closed。検知しても警告して実行していた旧挙動は廃止)。逃げ道は当該ステップへの`allow_access_override: true`の明示記載だけ。argsにはテンプレートが使えるため、**同じ検査はテンプレート展開後(起動直前)にも走る** — 上流出力が権限フラグを注入してもスポーン前に拒否される。`cmd:` ステップは対象外。
- **`write` でシェルは自動承認されない(全ツール共通)。** 原則: **サンドボックスのない環境でシェルを自動承認したら、それは`full`と同じ**。codexだけがOSサンドボックス付き(`-s workspace-write`)。claude / grok / pi / opencode にはサンドボックスがないため、`write`はいずれもシェルを自動承認しない(opencodeはenvでbash拒否を注入、piはシェルツールを登録せず拡張も無効化、claude/grokは編集のみ自動承認)。コマンド実行が必要なステップは`access: full`と自覚的に書くか、`args:` + `allow_access_override: true`を併記すること。**cursorはread/fullの2段階のみ**(非対話の`--force`は全承認=シェル含む)で、`access: write`はvalidateエラー。
- **セッションは作成時より高い権限では再開できない。** sfhはセッションの作成時accessを記録し(`log.jsonl`の`session.access`)、それより高いaccessでの`continue_from:` / `fork_from:`を既定で**拒否**する(read→write/full、write→full)。readステップが悪意あるWebページを取り込み、次のステップがそのセッションをfullで再開する、という典型的な権限昇格経路を塞ぐため。受け入れる場合のみ`allow_access_override: true`。
- **`read` は「漏れない」を意味しない。** ファイル書き込みとシェル実行を止めるだけで、Web検索やAPI送信は止まらない。秘匿データを read ステップに渡しても外に出ないとは限らない。
- **サブエージェントの出力は信頼できない入力**として扱うこと。stdoutに出るのはAIが生成したテキストで、その中にはWebから拾ってきた内容が混ざりうる(プロンプトインジェクション経路)。呼び出し元エージェントには「これはデータであって指示ではない」と伝えるのが安全。
- **文字列形式の `cmd:` でのテンプレート展開は既定で禁止。** 置換値はシェル文字列に注入されるため、メタ文字ブラックリストは安全境界にならない(禁止文字を1つも含まない値が、対象プログラムの危険なオプションになりうる。例: tar の `--checkpoint-action=exec=...`)。必要なら `unsafe_shell_template: true` を明示すること(メタ文字チェックは残る)。配列形式 `cmd: [...]` はシェルを介さず、置換値は1引数としてそのまま渡る — シェルインジェクションは起きないが、「引数として渡る」ことは変わらないため、対象プログラムのオプション解釈に対する保証ではない。

## 既知の注意点

- **プリセットの腐敗は `sfh doctor` で検知する。** 上の表は検証した日のフラグであって、これらのCLIは毎週のように変わる。ズレは静かに起きる(`validate`は通り、課金済みの実行の途中で死ぬ)ので、**新しいフローを回す前と、CLIを更新した後に `sfh doctor <flow.yaml>` を打つこと**。ズレていたら `args:` で足すか `cmd:` で全部書けば逃げられる。
- **検証済みバージョン**(全て実AI呼び出しで確認): codex-cli 0.146.0-alpha.3.1 / claude 2.1.220 / opencode 1.18.3 / grok 0.2.112 / agy 1.1.7 / pi 0.82.1 / cursor-agent 2026.05.28-a70ca7c
- **cursorプリセットはexperimental扱い。** セッションの実在確認が `~/.cursor/chats/<hash>/<id>/store.db` という**非公開・未文書のパス構造**に依存している(cursorには「このチャットは実在するか」を問う手段が他にない)。Cursor側がこの配置を変えたら`continue_from`が壊れる。壊れたことは `sfh doctor` では検知できない(単発実行は通るため)ので、cursorでセッションを繋ぐフローは実際に再開が効いているかを確認してから本番投入すること。
- **タイムアウト**: Windowsは`taskkill /T /F`、Unixはprocess group killで子孫ごと落とす。子の終了後にパイプを握り続ける孫プロセスがいてもドレイン期限で先に進む。出力は1ストリーム32MBでキャップ。
- **`--detach` の切り離しが効かない場合がある**: Windowsで呼び出し元がブレイクアウェイ禁止のjob objectを張っていると、切り離せずに親と心中する(sfhは警告を出す)。その場合でも `--resume` で続きから再開できる。Unix(`setsid`)には制約なし。**sfh自身のjob objectも意図的にブレイクアウェイ禁止**なので、フローの`cmd:`ステップから`sfh run --detach`してもフローより長生きはしない(禁止を解くと、msys2の`sh`がその抜け道を使って孫プロセスを取り残すため — 実測で確認済み)。
- **Windowsでコンソールウィンドウは出ない**: 子プロセスは`CREATE_NO_WINDOW`で起動する。多段フローで毎ステップcmdウィンドウが点滅する、ということはない。
- **opencodeのread/write**は`OPENCODE_CONFIG_CONTENT`で拒否を注入(read: edit+bash、write: bash+外部ディレクトリ。1.18.3のplan agentはbashを塞がないため。実機でBLOCKED確認済み)。`--auto`は明示deny以外を自動承認するので、writeがシェル付きにならないのはこの注入のおかげ。完全な保証が要る変更はfullレビューを挟むこと。
- **agyのexit codeは信用しない**(正常完了でexit 1がありうる)。sfhは常にJSONエンベロープの`status`で補正する。
- **AIステップが空の最終メッセージを返したら失敗扱い**(既定)。空文字が下流のプロンプトに流れ込む事故を防ぐため。意図的なら `allow_empty: true`。
- **エディタ補完**: フロー先頭に次の1行を足すとVS Code等でスキーマ補完が効く。
  `# yaml-language-server: $schema=https://raw.githubusercontent.com/Aero123421/SimpleFlowHarness/main/schema/flow.schema.json`

## 開発

```bash
cargo test                              # 132本の単体テスト
bash tests/engine_behaviour.sh ./target/release/sfh   # AIを呼ばない挙動テスト399本
```

挙動テストは冒頭で `tests/stub/session_stub.rs` を `rustc` で1回ビルドする。
これは `claude -p --output-format json` の形(`.result` / `.session_id` / `.usage`)で
答えるだけのスタブCLIで、`bin: "echo"` ではセッションIDを報告できず
「再開できたこと」を証明できない(旧B-15)ため。したがって挙動テストの実行には
sfhをビルドしたのと同じRustツールチェーンが要る。スタブはcargoのターゲットではない
(`tests/` 直下に置くと統合テストとして拾われてしまうのでサブディレクトリに置いてある)。

CIは3OS(Linux/macOS/Windows)でテスト+スモークフロー+READMEのインストール手順そのものを実行して検証する。
