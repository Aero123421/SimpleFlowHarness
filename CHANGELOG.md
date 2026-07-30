# Changelog

## v1.1.3 — 2026-07-30

### CLI と出力契約

- `sfh help <command>` と、引数の後ろに置いた `--help` を一貫して扱い、
  `run --help` から高度な `--run-dir` と `{{prompt_file}}` を発見できるようにした。
  `runs list --help` などの二段subcommandは固有のusageを維持する。
- `sfh plan -q/-v`、`sfh status -q`、`sfh stop -q` のように、受理しても動作を
  変えなかったoptionを黙って無視せず、通常のunknown flagとして拒否する。
- 人向けの`status`と成功した`stop`は、要約と次の操作を一つの順序付きstdout文書へ
  まとめた。成功した`wait`はフロー結果だけをstdoutへ返し、完了footerを別streamへ
  後置しない。自動処理向けの`status --json`契約は変更しない。
- detachはrun dirをflushしてから診断を出し、quiet設定をbackground childへも引き継ぐ。
  空の`runs list`が`$-0.0000`を表示する問題、`--limit`のエラーが`-n`を名乗る問題、
  `--older-than 30dd`を30日として受理する問題も修正した。成功した`runs list/clean`の
  人向けレポートもstdout内で完結する。

### 複雑フローの一貫性

- `foreach.split: json`は入力全体のJSONを優先し、説明文を含む場合は最後の完全で
  parse可能な配列を採用する。引用番号`[1]`、旧draft、JSON文字列内の括弧、
  nested array/objectを壊さず、最終構造化回答を決定的にfan-outする。
- 連続分岐のfallthrough警告をvalidationの副作用から分離し、通常runでは一度だけ、
  quiet runでは出さないようにした。`validate` / `validate --strict`は従来どおり
  問題を可視化・厳格化できる。
- 同梱`research.yaml`は調査・計画をread、成果物作成をwriteへ最小化し、
  verdictを完全一致で分岐する。反復上限では不完全な要約へ進まず`stuck`で人へ返す。

### インストールとOSS品質

- Release assetと同梱SHA-256を検証し、WindowsはユーザーPATH、Unixは
  `~/.local/bin`へ導入する手順を日英READMEへ追加した。Rust経由の導入もrelease tagへ
  pinし、古い`sfh`がPATHで優先される場合の確認方法を明記した。
- 英語READMEに終了コード、machine-readable status、waitのstdout契約を追加し、
  日本語READMEと利用者向けの運用情報を揃えた。
- CIでLinux/macOSのchecksum付き展開に加え、Windows PowerShellのchecksum・展開・
  PATH経由起動も実行する。公開Schemaと同梱例のversion URLをv1.1.3へ更新した。

## v1.1.2 — 2026-07-30

### 複雑フローの安全性

- `parallel` / `foreach` の各メンバーが終わった時点で `step_end` と成果物を同期し、
  遅い兄弟の実行中に sfh が停止しても完了済みメンバーを再実行・再課金しないようにした。
- `foreach` 開始時のordered item列を指紋化して記録し、resume時に変数・budget・OSなどで
  列が変わった場合は、同じindexの別itemを完了済みと誤認せず課金前に停止する。
- primary 失敗後に選ばれた fallback と、leaf / aggregate 完了後の compact / notes を
  独立した耐久 checkpoint にした。途中停止から同じ visit の未完了 stage だけを再開し、
  完了済み primary・fan-out・leaf を二重実行しない。
- compact は要約済み chain を同期してから完了イベントを記録し、notes は内容由来markerを
  付けてatomic更新する。後処理の途中停止でも要約器を再課金せず、同じvisitを重複追記しない。
- `parallel` / `foreach` の失敗メンバーもfallback profileを個別checkpointし、group再開時に
  そのprofileから続行する。完了済み兄弟だけでなく、失敗済みprimaryも再課金しない。
- 次の fallback が `max_total_steps` に阻止される場合も、直前の有料attemptを先に記録し、
  resumeでprimaryを再実行・再課金しない。
- ログ、status、meta、leaf/aggregate成果物の必須書込みを fail-closed に変更した。
  永続化できない状態を成功として先へ進めず、status 更新不能時は子プロセス群も停止する。
- 有料attemptの完了後に成果物を永続化できなかった場合も、そのattemptのtoken/costを先に
  累計し、`persistence_failure` を耐久ログへ記録する。この状態は外部副作用の成否が曖昧なため
  自動resumeを拒否し、同じ処理を無条件に再実行・再課金しない。
- resume時は`step_end` / `aggregate_end` / compactが指す成果物の存在と、記録済み
  `output_hash`を照合する。欠損・改竄・partial publishを空出力として続行しない。
- nonce、meta、leaf/aggregate/compact成果物は同期済み一時ファイルからatomic replaceし、
  resume中の再書込みや同時status/stop読取りがtruncate途中の内容を観測しないようにした。
- Windows でも既存 `status.json` を atomic replace して torn write を防ぎ、`sfh stop` は
  停止状態を永続化できなかった場合に成功を報告しない。Windows 10以降では
  `FileRenameInfoEx`のPOSIX置換を使い、status pollerが旧ファイルを開いている間も更新できる。
- Windowsでtimeout後もgrandchildがpipeを保持する場合に、live stdout teeのhandleが
  canonical成果物のatomic replaceを阻害しないようにした。timeoutを`on_error: continue`
  したflowも、永続化エラーに化けず次のstepへ進む。
- detach前の古いstatus削除や、spawn後のnonce永続化に失敗した場合は成功を返さず、
  既に起動したdetached process treeも停止して管理不能なrunを残さない。
- resume の照合対象を flow ファイルだけでなく、グローバル profile をマージした
  **実行で参照される実効設定**へ拡張した。tool/model/access/args/env/cwd の変化も既定で拒否し、
  その flow が参照しない machine-local profile の変更では resume を妨げない。
- `wall_clock_sec` を resume 前の経過時間から継続し、fan-out の待ち行列時間も各 leaf の
  deadline に含めた。各childの残時間は、成果物の安全確認を終えたspawn直前に再計算する。
- Ctrl+C / `sfh stop` による割り込みは、`on_error`・route・compact・notesより常に優先する。
  `on_error: goto:end` がユーザーの停止要求を成功へ変換する経路をなくした。
- 改竄・破損logが`u32`上限のvisitを記録していても、次visitを0へwrapせず
  resumeをfail-closedで拒否する。
- compact summarizer が失敗・timeout・空出力になった場合も、外部 CLI が報告したコストを
  捨てずに run 合計と `max_cost_usd` 判定へ反映する。
- providerが複数messageで極端なtoken数を返してもusage加算をwrapさせず、`u64`上限で
  saturateして過少報告を防ぐ。

### 検証と診断

- `api_version: 1`、全 terminal ID の予約、catch-all 最終規則、Schema と同じ数値下限、
  tool/profile/compact 設定の静的検証を追加した。
- 制御フローグラフ上の dominance を検査し、未実行になり得る session source や
  `steps.<id>.*` 参照を実行前に拒否する。意図的な分岐依存には
  `| optional` / `| default:text` を明示できる。
- fallback ごとに明示的な access を解決し、case違い・重複profileによる成果物衝突を拒否する。
  session source は親parallel groupもfail-closedで、全fallbackが同じproviderであることを検証する。
- `sfh validate --strict --json`、`sfh plan`、`sfh graph [--mermaid]`,
  `sfh config show`、`sfh runs why` を追加した。各 subcommand の `--help` も独立して動く。
- `sfh config show` はprofile/default/stepの環境変数値を既定で`<redacted>`に伏せ、
  実値表示には機密出力であることを警告する`--show-secrets`を必須にした。
- `sfh plan` は未実行の上流出力とnotesを可視placeholderで解決する。上流結果だけをpromptに
  渡す正当な後続stepも「空prompt」と誤判定せず、複雑flow全体を最後まで表示する。
  parallel child / foreach memberのfallbackとcompact summarizerも省略せず解決・表示する。
- `sfh plan` は `--resume` / `--detach` / `--emit` など実行専用optionを受理しない。
  副作用のない計画表示という契約に無関係なflagを黙って流用しない。
- `--resume` と `--resume-latest`、`--quiet` と `--verbose` のような相反するoptionを
  明示エラーにし、`--force-resume` 単独指定も黙って無視しない。
- graph は通常routeだけでなく `on_error` / `on_max_visits` の fail・continue・goto と
  global `on_budget` も表示する。`sfh runs` は各操作に無関係なflag/位置引数を黙って無視しない。
- status に fan-out の総数・完了数・active member を追加し、`sfh status` から長い並列処理の
  内訳を確認できるようにした。
- `log.jsonl` と `status.json` に `schema_version: 1` を付け、公開 JSON Schema を追加した。

### OSS と互換性

- `api_version` を省略した既存 flow は v1 として読み続ける。v1.1.2 の同梱例はすべて明示する。
- Windowsでは実行ファイルではないshell builtinの`echo`を配列commandにしていたAI不要の
  smoke examplesを、3 OSでそのまま走る固定文字列commandへ変更した。
- `end` / `fail` / `stuck` は terminal と同名の step が経路解釈を曖昧にしないよう、
  大文字小文字を区別せず step id として拒否する。
- 英語 README、貢献・サポート・セキュリティ・行動規範、Issue/PR テンプレート、
  Rust toolchain/MSRV、依存更新設定を追加した。
- CI/release Actionsをcommit SHAへ固定し、通常jobのtokenをcontents readへ縮小した。
  tag・Cargo package version・CHANGELOGが一致しないreleaseもbuild前に拒否する。
- `cargo-deny` をCIへ追加し、既知advisory・license・重複/禁止crate・未知sourceを
  `deny.toml` の公開policyに照らしてrelease前に監査する。
- process tree停止のplatform境界を明記し、Windowsではkill-on-close jobの作成・設定・
  child割当てを確認できない場合に、未管理の有料processを走らせずspawn直後にfail-closedする。
  process全体jobに加えてleafごとのnested jobを持ち、timeoutしたleafのgrandchildを即時停止しつつ
  実行中のparallel siblingは停止しない。
- root commandが正常終了してもbackground子孫をleafの所有processとして回収する。
  継承されたstdout/stderr pipeでdrainが停止したり、正常終了後にprocessが残る経路をなくした。
- `max_cost_usd` は外部ツールが**報告した確定コスト**に対するガードであり、未報告の
  実行中支出を事前予約する hard billing cap ではないことを文書化した。

## v1.1.1 — 2026-07-29

### 修正

- 再試行した leaf のトークン数・報告コストを全 attempt 分累積するようにした。失敗した
  attempt の支出が最後の成功値で上書きされず、`max_cost_usd` をすり抜けない。
- 外部ツールが返す負数・NaN・無限大のコストを会計境界で正規化した。負数は過去の支出を
  払い戻さず、正の無限大は有限予算を fail-closed で止める。`max_cost_usd` 自体も
  finite かつ 0 以上でなければ validate が拒否する。
- 失敗した probe / fan-out が `step_end` / `aggregate_end` を記録した直後に停止した run を
  resume するとき、`on_error: continue` / `goto:*` と route だけを再生するようにした。
  外部 probe を二重実行せず、記録済み exit / stderr / 出力をそのまま使う。
- `tool_max_parallel: 0` を validate で拒否し、直接生成された `ToolGate` も 1 に丸める。
  child spawn 前に永久待機する経路をなくした。
- `max_total_steps` を primary だけでなく fallback と compact の leaf run にも適用した。
- `parallel:` group に置かれた leaf 専用の `retry_on` / `hang_after_sec` を黙って無視せず
  validate エラーにした。
- process 終了と pipe reader の競合で最後の出力を見落とし、`idle_ms` を過大計上して
  正常終了を hang 扱いする可能性を修正した。

### リリース品質

- release は 3 OS の全 CI と install 手順検証を通過してから build し、全 5 platform の
  asset と checksum が揃った後に一度だけ公開する。matrix の途中失敗による partial
  release を防止した。
- CI の Clippy を `--all-targets -D warnings` に統一した。
- v1.1 仕様書の実装済み phase / checklist、README のテスト記録を同期し、
  ローカル `.claude/worktrees/` を追跡対象から除外した。

## v1.1.0

### 破壊的変更

1. **step id `stuck` を予約語にした(大文字小文字を区別しない)。** 第 3 の終端
   `goto: stuck`(exit 4)を追加したため、`id: stuck` / `id: STUCK` を持つ既存フローは
   `sfh validate` が明示エラーで拒否する。黙って動作が変わることはない。
2. **`on_budget` は宣言した上限ごとに 0 でない `budget_reserve` を要求する。**
   reserve が 0 だと着地の閾値が上限そのものになり、着地した次のループ先頭で
   上限検査が同じ値で発火して、着地連鎖が 1 ステップも走らないまま従来のエラーで
   終わる。validate が拒否する。
3. **`parallel:` の子の `on_error:` は `fail` / `continue` のみ。** `goto:end` /
   `goto:fail` / `goto:stuck` は以前は validate を通ったが実行時に無視されていた
   (`continue` かどうかしか見ていない)。頼んだ終端と違う exit が返るより、
   validate で落とす。
4. 新キーを含むフローは v1.0 の sfh では読めない(`deny_unknown_fields` により
   大声で拒否される)。追記互換なのはログ側だけ。

### 追加

- **`when_members`**: `parallel:` / `foreach:` メンバーの票を数えて分岐する route 述語
  (`last_line_is` + `at_least: <n>` または `all: true`)。票は文字列ではなく
  engine が持つ成否から数えるので、「合格と書いた上で落ちたメンバー」は数えない。
- **idle 二時計**: 「経過時間」と「最終出力からの時間」を分けて記録する。
  `step_end.idle_ms`、status.json の `last_output_utc` / `step_started_utc` / `visit`。
  タイムアウトのうち沈黙を伴うものだけを「ハング」= 一過性として再試行する
  (`hang_after_sec`、既定 300)。
- **`goto: stuck`(exit 4)**: 「作業は保存されているが人間の判断が要る」を機械可読にする終端。
  `sfh status` / `sfh wait` も 4 を返す。stuck な run は failed と同様に再開できる。
- **ログ拡充**: `position` に `rule` / `route_line`(+ `votes` / `voters`)、
  `step_start` に `session_parent`(fan-out メンバーは `parent` 付きで各自記録)、
  `step_end` に `os` / `idle_ms`、`aggregate_end` に `members[]`。
- **`when_exit` / `when_stderr_matches`**: 「正しい理由で落ちた」を判定する route 述語。
- **`on_budget` / `budget_reserve`**: 上限の手前で着地連鎖へ跳ぶ(1 run 1 回)。
- **examples/cross-os-gate.yaml**: サポートを主張する OS ごとに 1 ゲート置くための実例。

### 修正

- `when_stderr_matches` のテンプレートが `precheck` に入っておらず、変数の打ち間違いが
  `sfh validate` と `--dry-run` を通り抜けて、ステップを実行・課金した後の
  route 評価で初めて落ちていた。
- resume した pending route で `when_members` を数える母数が、記録側のメンバー数だった。
  フローを編集してメンバーを増やし `--force-resume` すると、増えた 1 体に聞かないまま
  `all: true` が成立していた。現在は記録と宣言の数が食い違えばエラーで止まる。
- メンバーの判定行を 200 字で切った結果が前方一致になり、判定文字列がちょうど 200 字の
  とき「判定文字列を言ってからさらに喋ったメンバー」を票に数えていた。切った行
  (`members[].clipped: true`)は数えない。
