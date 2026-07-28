# v1.1 バックログ — v1.0 のブロッカーにしないと決めた指摘

2026-07-28 の強化ラウンドで codex 3体が挙げた指摘のうち、v1.0 の合格条件から
外したもの。外した理由は「不要だから」ではなく、**攻撃面レビュアーに与えた問いに
終端条件が無かった**ため。「まだ回避経路はあるか」は、サンドボックスを持たず外部CLIを
起動するプログラムに対しては常に yes になる。合格条件は列挙した項目に限定し、
ここに書いたものは次のラウンドで個別に取り上げる。

## 攻撃面レビュアー(rev_break)の全指摘
```
以下の未達があります。

1. Windows の hidden traversal 検査を回避できます。[src/contain.rs:104](D:/AI/SimpleFlowHarness/src/contain.rs:104)、[src/contain.rs:126](D:/AI/SimpleFlowHarness/src/contain.rs:126)  
   `".. "` は `trim()` 後に `".."`、続く `trim_end_matches('.')` で空文字になり検出されません。`foo. ` なども de-verbatim 後に別名へ正規化されます。検査時は run 内でも、cmd/MSYS 等が通常パスを再解釈すると親ディレクトリや外向き junction を参照できます。

2. Windows の通常書込みが reparse point を追います。[src/contain.rs:257](D:/AI/SimpleFlowHarness/src/contain.rs:257)、[src/contain.rs:282](D:/AI/SimpleFlowHarness/src/contain.rs:282)  
   `write_private` / `append_private` の Windows 分岐には no-follow がありません。`sfh-nonce`、`log.jsonl`、`notes.md` 等を外向き symlink にすると、[src/engine.rs:1138](D:/AI/SimpleFlowHarness/src/engine.rs:1138)、[src/engine.rs:1161](D:/AI/SimpleFlowHarness/src/engine.rs:1161) 等から run 外を上書き・追記できます。

3. stdout tee は全OSで symlink を追って外部ファイルを上書きします。[src/execute.rs:743](D:/AI/SimpleFlowHarness/src/execute.rs:743)、[src/leaf.rs:1027](D:/AI/SimpleFlowHarness/src/leaf.rs:1027)  
   予測可能な `<tag>.out.txt` を symlink にすると、`File::create` がリンク先を truncate し、子プロセス出力を書き込みます。後段の `write_private` が失敗しても損傷済みです。

4. Codex の `<tag>.last.txt` も未封じ込めです。[src/preset.rs:876](D:/AI/SimpleFlowHarness/src/preset.rs:876)、[src/preset.rs:1111](D:/AI/SimpleFlowHarness/src/preset.rs:1111)、[src/leaf.rs:825](D:/AI/SimpleFlowHarness/src/leaf.rs:825)  
   run 内 symlink が `--output-last-message` の書込み先として外部CLIへ渡され、その後 sfh も通常の `read_to_string` で追います。run 外の上書きまたは内容の chain output への混入が可能です。

5. canonicalize と再openの間に TOCTOUがあります。[src/contain.rs:159](D:/AI/SimpleFlowHarness/src/contain.rs:159)、[src/contain.rs:201](D:/AI/SimpleFlowHarness/src/contain.rs:201)、[src/watch.rs:442](D:/AI/SimpleFlowHarness/src/watch.rs:442)  
   検査後に中間ディレクトリや最終ファイルを外向き symlink/junctionへ交換すると、resume または `wait` が run 外を読みます。既存の任意 resume dir は0700とは限らず、WindowsではDACLも設定されません。

6. 固定管理ファイルが封じ込め検査を通っていません。[src/engine.rs:304](D:/AI/SimpleFlowHarness/src/engine.rs:304)、[src/engine.rs:1015](D:/AI/SimpleFlowHarness/src/engine.rs:1015)、[src/watch.rs:100](D:/AI/SimpleFlowHarness/src/watch.rs:100)、[src/watch.rs:350](D:/AI/SimpleFlowHarness/src/watch.rs:350)  
   `log.jsonl`、`meta.json`、`status.json`、`sfh-nonce` 自体が外向き symlinkでも直接開かれます。外部JSON/JSONLをresume状態、vars、session、PID、終了状態として取り込めます。

7. target省略時と `--resume-latest` が runs root 外の symlink/junctionを選べます。[src/watch.rs:55](D:/AI/SimpleFlowHarness/src/watch.rs:55)、[src/engine.rs:630](D:/AI/SimpleFlowHarness/src/engine.rs:630)  
   `is_dir()` / `exists()` はリンクを追いますが、選択結果の canonical root containment がありません。`status` / `wait` / `stop` / `--resume-latest` が root 外のディレクトリを処理します。

8. nonce はrunに結び付いておらず、PID再利用にも耐えません。[src/contain.rs:178](D:/AI/SimpleFlowHarness/src/contain.rs:178)、[src/watch.rs:350](D:/AI/SimpleFlowHarness/src/watch.rs:350)、[src/execute.rs:478](D:/AI/SimpleFlowHarness/src/execute.rs:478)  
   同じ未信頼ディレクトリ内で `(PID, nonce)` を二重記録するだけです。稼働中runをコピーするとコピー側から元PIDをstopできます。古いPIDが別の同名`sfh`へ再利用された場合も、完全stem一致は通るため無関係なプロセスをkillします。process start timeやhandle/pidfdは保持されていません。

9. nonce/PIDの不正値がfail-openします。[src/contain.rs:189](D:/AI/SimpleFlowHarness/src/contain.rs:189)、[src/watch.rs:351](D:/AI/SimpleFlowHarness/src/watch.rs:351)、[src/watch.rs:365](D:/AI/SimpleFlowHarness/src/watch.rs:365)  
   nonce内PIDのparse失敗はエラーではなく`None`となり、PID照合が省略されます。nonce読取り失敗も`.ok()`で欠落扱いされ、両側欠落なら`log.jsonl.exists()`だけでlegacy runとして許可されます。また [src/watch.rs:122](D:/AI/SimpleFlowHarness/src/watch.rs:122) は範囲外PIDを`u32`へ切り詰めます。

10. `wait` / `status` に成功判定のfail-openが残っています。[src/watch.rs:188](D:/AI/SimpleFlowHarness/src/watch.rs:188)、[src/watch.rs:506](D:/AI/SimpleFlowHarness/src/watch.rs:506)、[src/watch.rs:560](D:/AI/SimpleFlowHarness/src/watch.rs:560)  
    `status` はnonceを検査せず、偽の`state:"done"`でexit 0です。`wait`はnonce検査が`done`だけなので、`failed` / `dead` / `stopped`と`exit_code:0`で成功0を返します。`done`でもnonce両欠落＋空の`log.jsonl`でlegacy分岐を通ります。さらに結果出力がnonce検査より先です。

11. session accessを未信頼logの自己申告値で判定しています。[src/engine.rs:547](D:/AI/SimpleFlowHarness/src/engine.rs:547)、[src/engine.rs:561](D:/AI/SimpleFlowHarness/src/engine.rs:561)、[src/leaf.rs:483](D:/AI/SimpleFlowHarness/src/leaf.rs:483)  
    欠落・不正accessは概ね拒否しますが、有効な偽`"access":"full"`はそのまま採用されます。実際にはreadで作られたsessionでも、fullでの`continue_from` / `fork_from`が昇格なしと判定されます。

12. run由来値のtaintが privileged template sinkまで維持されません。[src/engine.rs:1015](D:/AI/SimpleFlowHarness/src/engine.rs:1015)、[src/leaf.rs:269](D:/AI/SimpleFlowHarness/src/leaf.rs:269)、[src/leaf.rs:345](D:/AI/SimpleFlowHarness/src/leaf.rs:345)  
    `meta.json.vars`、`notes.md`、foreachの`item`はrun由来ですが、`bin` / `cwd`検査はキー名が`steps.`で始まる場合しか拒否しません。crafted runにより任意の実行ファイルや作業ディレクトリを選択できます。argv形式の`cmd`はargv[0]まで通常展開され、[src/leaf.rs:417](D:/AI/SimpleFlowHarness/src/leaf.rs:417) から任意プログラム起動へ到達します。

13. argv形式に包んだshellがshell-template防御を迂回します。[src/flow.rs:907](D:/AI/SimpleFlowHarness/src/flow.rs:907)、[src/leaf.rs:384](D:/AI/SimpleFlowHarness/src/leaf.rs:384)、[examples/v1-harden-r2.yaml:57](D:/AI/SimpleFlowHarness/examples/v1-harden-r2.yaml:57)  
    `cmd: ["sh","-c","...{{untrusted}}..."]` はargv分岐なので、`unsafe_shell_template`やメタ文字検査を通りません。しかし第3引数は実際にはshellに再解釈されます。resumeした`meta.json.vars`や過去runの内容からshell injection・任意ファイル読取りが可能です。

14. resume指定だけでaccess必須検査がlenientへ落ちます。[src/engine.rs:971](D:/AI/SimpleFlowHarness/src/engine.rs:971)、[src/flow.rs:244](D:/AI/SimpleFlowHarness/src/flow.rs:244)、[src/flow.rs:848](D:/AI/SimpleFlowHarness/src/flow.rs:848)  
    strict load失敗時、runが本当にlegacyか認証せず、欠落accessを`write`として実行します。根拠となるfingerprintも同じ未信頼`meta.json`の自己申告なので、crafted run dirでfresh実行なら拒否されるflowをwrite権限で開始できます。

15. resume logの他の成功フィールドも欠落時にfalse扱いです。[src/engine.rs:439](D:/AI/SimpleFlowHarness/src/engine.rs:439)  
    `exit`だけは`Some(0)`必須ですが、`timed_out`、`interrupted`、`failed`は欠落・型不正を`false`にします。`exit:0`だけの偽`step_end`が成功として復元され、stepの再実行を省略し、session・output・routeを採用します。

16. session再開確認も報告フィールド欠落時に許可します。[src/leaf.rs:767](D:/AI/SimpleFlowHarness/src/leaf.rs:767)、[src/leaf.rs:783](D:/AI/SimpleFlowHarness/src/leaf.rs:783)、[src/leaf.rs:1075](D:/AI/SimpleFlowHarness/src/leaf.rs:1075)  
    session ID/marker比較は期待値と報告値が両方`Some`の場合だけです。CLIがsession情報を返さなくてもpreassigned/expected IDへfallbackし、正しいsessionを再開・forkしたものとして成功扱いできます。

REVIEW-FAIL```

## 回帰レビュアー(rev_regression)の全指摘
```
以下の回帰があります。テストは実行せず、差分とコードのみを確認しました。

- 高: parallel / foreach の途中で落ちると、完了済みメンバーまで再実行されます。[engine.rs](/D:/AI/SimpleFlowHarness/src/engine.rs:410) は完了済み子の回数・コストを復元し、親グループを再開位置にしますが、再開時は [parallel](/D:/AI/SimpleFlowHarness/src/engine.rs:1542)・[foreach](/D:/AI/SimpleFlowHarness/src/engine.rs:1686) の全メンバーを再生成します。重複課金・別 session の生成に加え、`max_total_steps` がライブ実行に必要な件数ちょうどの場合、復元済み件数＋全メンバー件数で上限超過となり、再開不能です。

- 高: 旧 run の `continue_from` / `fork_from` は write/full で再開できません。旧ログには `session.access` がないため [load_resume](/D:/AI/SimpleFlowHarness/src/engine.rs:579) が `None` を復元し、[prepare_leaf](/D:/AI/SimpleFlowHarness/src/leaf.rs:506) は read 以外を拒否します。旧フローに明示的な `access: write` があった場合もログ側は欠落しているため同じです。新規ライブ実行では access が記録されるので、明確なライブ／resume 不一致です。またエラーは `allow_access_override` の追加だけを案内しますが、フロー編集後はさらに `--force-resume` が必要になります。

- 中: 旧 run 互換ローダーが、新しい文字列形式 `cmd:` 制限を緩和していません。[load_lenient](/D:/AI/SimpleFlowHarness/src/flow.rs:244) も通常の検証を呼び、[テンプレート入り文字列 cmd の拒否](/D:/AI/SimpleFlowHarness/src/flow.rs:916) は無条件です。したがって旧版で正当だった `cmd: "echo {{steps.x.output}}"` を含む未完了 run は、FNV 指紋が一致しても再開できません。案内どおりフローを直すと指紋が変わるため、やはり `--force-resume` が別途必要です。

- 中: `bin:` / `cwd:` の step-output テンプレートは `sfh validate` を通る一方、実行時に必ず拒否されます。[prepare_leaf](/D:/AI/SimpleFlowHarness/src/leaf.rs:345) が `steps.*` を全面禁止していますが、同じ静的検査が validator にありません。信頼済み判定コマンドが作った worktree パスを次段の `cwd:` に使う正当な構成も拒否され、明示的な escape hatch もありません。上流ステップ実行・課金後に初めて失敗し得ます。

R-1、R-3〜R-7 の指定された問題はコード上修正されています。R-2 の FNV→SHA-256 判定自体も修正されていますが、上記の旧 session と旧 `cmd:` のため、旧 run 互換性全体としては未修正です。

REVIEW-FAIL```

## 網羅性レビュアー(rev_complete)の最終判定
```
コードとテストソースを照合した結果、S2-4 に要件違反が残っています。

| 項目 | 判定 | 根拠 |
|---|---|---|
| S1-1 `wait` | 完了 | `detached.out.txt` を canonicalize 相当の包含確認後に読み、違反時は `1` を返す。[watch.rs:439](D:/AI/SimpleFlowHarness/src/watch.rs:439)、[watch.rs:509](D:/AI/SimpleFlowHarness/src/watch.rs:509)。外向き symlink、情報非出力、非ゼロ終了を確認。[engine_behaviour.sh:873](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:873) |
| S1-2 `stop` | 完了 | 実行ファイルの stem を部分一致ではなく等値比較。[execute.rs:481](D:/AI/SimpleFlowHarness/src/execute.rs:481)。`*-helper` の拒否テストあり。[execute.rs:912](D:/AI/SimpleFlowHarness/src/execute.rs:912)。一致 nonce/PID を持たせた無関係な実プロセスについて、生存を `kill -0` で確認し、kill メッセージは stderr を検索。[engine_behaviour.sh:1025](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1025) |
| S1-3 flow name | 完了 | `flow::validate` が `validate_name` を呼ぶ。[flow.rs:469](D:/AI/SimpleFlowHarness/src/flow.rs:469)。Schema に区切り文字、制御文字、`.`/`..` などの制約あり。[flow.schema.json:10](D:/AI/SimpleFlowHarness/schema/flow.schema.json:10)。validate/run 双方の拒否テストあり。[engine_behaviour.sh:1904](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1904) |
| S1-4 resume | 完了 | `chain_file` と `precompact_file` は `read_contained_opt(...)?`、`out_file` は `contained_opt(...)?` で、エラーを伝播。[engine.rs:344](D:/AI/SimpleFlowHarness/src/engine.rs:344)、[engine.rs:450](D:/AI/SimpleFlowHarness/src/engine.rs:450)、[engine.rs:483](D:/AI/SimpleFlowHarness/src/engine.rs:483)。絶対 `out_file`、絶対 `precompact_file`、外向き symlink で resume が非ゼロになるテストあり。[engine_behaviour.sh:2049](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:2049) |
| S2-4 session access | **不足** | write/full への不明 access 再開は拒否するが、記録 access が欠落・不正で `None` の場合でも、再開先が `read` なら override なしで許可する。[leaf.rs:495](D:/AI/SimpleFlowHarness/src/leaf.rs:495)。さらにその fail-open 動作を明示的に成功期待するテストがある。[leaf.rs:2129](D:/AI/SimpleFlowHarness/src/leaf.rs:2129)。指定された「欠落・不正なら `allow_access_override: true` なしでは fail-closed」を満たさない。 |
| S3-3 `.gitignore` | 完了 | 書き込み失敗と再読込失敗を返し、書き込み後の内容も再検証。[engine.rs:759](D:/AI/SimpleFlowHarness/src/engine.rs:759)。書き込み不能時の非ゼロ終了テストと、再検証述語のテストあり。[engine_behaviour.sh:2186](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:2186)、[engine.rs:2834](D:/AI/SimpleFlowHarness/src/engine.rs:2834) |
| R-1 | 完了 | Windows、Linux、macOS それぞれで実行パスを取得し、macOS は `proc_pidpath` を使用。[execute.rs:397](D:/AI/SimpleFlowHarness/src/execute.rs:397)、[execute.rs:424](D:/AI/SimpleFlowHarness/src/execute.rs:424)、[execute.rs:440](D:/AI/SimpleFlowHarness/src/execute.rs:440)。実 detached run の stop テストは3 OSのCI対象。[engine_behaviour.sh:1964](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1964)、[ci.yml:13](D:/AI/SimpleFlowHarness/.github/workflows/ci.yml:13) |
| R-2 | 完了 | 記録された `sha256`/`fnv1a` を読み分け、algo 欠落は旧 FNV として検証。[engine.rs:839](D:/AI/SimpleFlowHarness/src/engine.rs:839)。正しい旧FNV runを `--force-resume` なしで再開するテストあり。[engine_behaviour.sh:1526](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1526) |
| R-3 | 完了 | nonce が双方欠落し `log.jsonl` が存在する場合のみ旧形式として認識し、その後も実行ファイル確認を必須化。[watch.rs:350](D:/AI/SimpleFlowHarness/src/watch.rs:350)、[watch.rs:409](D:/AI/SimpleFlowHarness/src/watch.rs:409)。偽 status、無関係プロセス、実 sfh プロセスの正負テストあり。[engine_behaviour.sh:1744](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1744) |
| R-4 | 完了 | nonce は親で一度生成し、detached 子へ `SFH_NONCE` で継承。[engine.rs:1120](D:/AI/SimpleFlowHarness/src/engine.rs:1120)。status/nonce の一致観測と即時 stop テストあり。[engine_behaviour.sh:1983](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1983) |
| R-5 | 完了 | 引数値を access tier と比較し、現在の access より広い場合だけ拒否。[preset.rs:465](D:/AI/SimpleFlowHarness/src/preset.rs:465)。read-only の正常系、danger-full-access の拒否、誤った `access: full` 案内がないことを確認。[engine_behaviour.sh:1849](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1849) |
| R-6 | 完了 | Unicode・空白・途中のピリオドを許可し、経路に影響する名前を拒否。[flow.rs:433](D:/AI/SimpleFlowHarness/src/flow.rs:433)。validate/run の一致を正負双方でテスト。[engine_behaviour.sh:1893](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1893) |
| R-7 | 完了 | `mkdir_private` は今回作成したディレクトリだけを `0700` に変更。[contain.rs:326](D:/AI/SimpleFlowHarness/src/contain.rs:326)。既存 `0770` root の維持と、新規 run dir の `0700` を確認。[engine_behaviour.sh:1939](D:/AI/SimpleFlowHarness/tests/engine_behaviour.sh:1939) |

不足は S2-4 の1点です。欠落・不正 access を表す `None` は、再開先が `read` の場合も明示的 override なしでは拒否する必要があり、現在の成功期待テストも拒否期待へ変更する必要があります。

REVIEW-FAIL```
