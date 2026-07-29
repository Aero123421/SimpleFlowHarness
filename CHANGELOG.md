# Changelog

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
