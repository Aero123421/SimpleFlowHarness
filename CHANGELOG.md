# Changelog

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
