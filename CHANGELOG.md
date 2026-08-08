# Changelog

## v1.5.0 — 2026-08-08

v1.4.0の深掘りreviewへの対応です。見つかった問題はengineの基本制御ではなく、
**sfhと外の世界との境界**に集中していました。「sfhが何を保証したと言っているか」と
「実際に保証できているか」がずれている箇所、と言い換えられます。

flow fileの書き方は変わりません。既存flowはそのまま動きます。

### `access:` が閉じていない範囲まで「enforced」と表示していました

- **これは何が起きたか。** `AdapterInfo`はclaude/opencode/grok/piのread/writeを
  `Enforced`と報告し、preflightがそれを表示し、SECURITY.mdもその表示に寄りかかって
  いました。しかしpresetが実際に閉じていたのは**adapter作者が列挙したbuiltin tool
  だけ**です。MCP toolは別のpermission名前空間、plugin・hook・skillはallowlistの
  外側、instruction fileは無検査で読み込まれます。opencodeの`--auto`に至っては
  **deny listが挙げなかった能力を承認します**。
- `Enforced`の基準をenumのdoc commentに明文化しました。「CLI自身がそのaccess
  class全体を閉じると保証するflagをsfhが渡している」ことであって、「presetが
  たまたま知っていたtoolをdenyした」ではありません。
- この基準に照らして claude / opencode / grok / pi / cursor / agy を
  `BestEffort` へ下げました。実OS sandboxでprocessごと囲うcodexだけが
  `Sandboxed`のまま残ります。**現在`Enforced`を名乗るadapterはありません。**
- `known_gaps`は総論をやめ、adapterごとに閉じられていない面を名指しします。
- piのstrict presetに`--no-context-files`を追加しました。read/writeの線引きは
  「piがどのtoolを使ってよいか」であって「disk上のfileがinstructionを注入して
  よいか」ではないので、制限側の2 tierとも抑止します。
- **grokの`--no-auto-update`は追加していません。** pinしたgrok CLIでこのflag名を
  確認できませんでした。確認できないflagを足すのは、この修正が正そうとしている
  誤りそのものです。`known_gaps`に記録し、勝手に「直され」ないようtestで固定して
  います。

### 32 MiBを超えるstructured streamが、terminal recordごと落ちていました

- **これは何が起きたか。** `OutputObserver`はstdoutを流れながら受け取れる設計で、
  raw captureの32 MiB上限より手前に立っています。しかしsemantic observerを持って
  いたのは**piだけ**でした。Codex JSONLもOpenCode NDJSONも、process終了後に
  **上限で切られた**`stdout_clean`をparseしていました。
- 結果として大きなstreamでは、terminal recordが消えて正常runがprotocol
  invalidになり、session idが消えてresume/forkが迷子になり、usage recordが消えて
  **costを過少計上**し、途中のerror recordが消えて失敗が成功として報告され得ました。
- Codex JSONLとOpenCode NDJSONにもstreaming observerを付けました。行分割の共通部分は
  `LineStreamObserver`に括り出し、record解釈だけadapterごとに残しています。同じ
  accumulatorをstreaming pathと非streaming pathの両方が使うので、adapterごとの
  意味論の実装は1つです。
- 1 recordあたり16 MiBの上限を追加し、超過はfail-closedです。protocolが無制限に
  bufferさせられる余地を残しません。

### run途中でcontext fileを書き換えると、記録と実際に渡したものがずれました

- **これは何が起きたか。** execution closureはrun開始時にcontext fileを**内容で**
  pinします。ところがstep準備時の`context::build`は**元のpathを開き直して**いました。
  両者をstep起動前に突き合わせる処理はありません。つまりanalyze stepのあとに
  `TASK.md`を書き換えると、implement stepは新しい内容を読み、
  `execution-closure.json`は古いhashのままです。resumeを待たずにprovenanceが壊れます。
- run開始時に`kind: file`のcontextをrun dirへsnapshotし、**各stepはsnapshotを読みます**。
  closureがpinしたbytesとmodelが見たbytesが、構造として同じものになります。
- inline/templateは対象外です。templateは`{{steps.x.output}}`を参照でき、step
  ごとに変わることが仕様なので、凍結してはいけません。
- **resumeは`--force-resume`でも元のsnapshotを使い続けます。** closureは
  write-onceなので、resume時に取り直すと「新しいbytesを、古いhashを記録したclosureの
  下で凍結する」ことになり、この修正が消そうとしている食い違いを作り直してしまいます。
- snapshotを書けない場合はpersistence failureです。live読み込みへ黙って戻ることは
  しません。戻ることこそがこのbugだからです。

### `preflight` が任意の `bin:` を実行していました

- **これは何が起きたか。** `cmd:`のprogramをpreflightが**resolveするだけで実行しない**
  理由は、code中にそう書いてあります —「`deploy.sh --help`はdeployしかねない」。
  この理屈がpreset toolの`bin:` overrideには適用されていませんでした。`bin:`は任意の
  pathを取れるので、`sfh preflight flow.yaml`が**そのflowの選んだprogramを実行**します。
  未信頼のflowが安全か確かめるためのcommandが、まさにその確認の場でcodeを走らせて
  いたことになります。AI生成flowに対して特に危険です。
- 線引きはPATH上で解決したtool自身の既定launcherです。sfhが同梱supportしていて
  `--help`/`--version`が無害だと確認済みのものだけを実行します。それ以外の`bin:`は
  resolveするだけで、`--probe-binaries`を明示した場合にのみ実行します。
- 沈黙が「問題なし」に読めてはいけないので、`ProbeState`が区別します。従来の
  `version: null`は「実行したが読めなかった」と「そもそも実行していない」の両方を
  意味していました。JSONは`probe_state`と`probe_binaries`を、人向け出力は
  「not probed」と該当flagを示します。
- 実行するprobeもisolated scratch directoryから走らせます（`doctor`と同じ理由です）。
  作れない場合は何もprobeしません。「isolationなしでprobeした」にはしません。

### parallel childのreplay policyが、warningから見えていませんでした

- **これは何が起きたか。** `replay_summary`と`replay_warnings`はtop-level stepだけを
  走査していました。一方runtimeは未完了のchildをidで引き、**そのchild自身の**policyを
  適用します。したがって`effects: external` + `replay.unfinished: rerun`をchildに
  書いたflowは、resumeで外部送信をやり直すのに、`plan`も`validate --strict`も
  何も言いませんでした。重複した外部作用を止めるために書かれたwarningが、まさに
  その場面で黙っていたわけです。
- 走査を`all_steps`ひとつに集約し、childは`parent.child`という既存の命名で現れます。
  他のstep走査も監査しました。多くは既に再帰済みで、再帰していないもの
  （concurrency ceiling、control-flow graph）は`parallel:`内に`route:`や入れ子を
  禁じているため正しく、その旨をコメントに残しました。

### carry元の「終わっている」確認が、status.jsonを読めないときfail-openでした

- **これは何が起きたか。** carryは`running`なsourceを拒否しますが、status.jsonが
  無い・読めない場合は「どちらとも言えないので止めない」でした。まだlogへ
  append中のrunからcarryでき、取ったsnapshotは直後に無効になり、双方の数字が
  検出不能なまま狂います。
- carryには**停止の積極的な証明**を要求します。durableな`run_end` eventがあるか、
  run自身のnonceでowner processのdeathを確認したうえでlogの最終位置がterminalか。
  拒否がないことは、もはや「はい」ではありません。

### carryしたactive timeが、status.json頼みでした

- **これは何が起きたか。** `run_end`はleaf_runsとcostを持つのにelapsedを持たず、
  carry/resumeはwall-clockをstatus.jsonから復元していました。status.jsonが欠けたり
  古かったりすると、A→B→Cの中間runの時間が消え、`wall_clock_sec`が実質resetします。
- `run_end`に`elapsed_sec`を記録し、復元順序を
  **`run_end` → `meta` → `status`** と明示して、使用箇所にその理由を書きました。

### `runs list` の行とtotalが別の量でした

- **これは何が起きたか。** 行の`cost_usd`は引き継ぎ込みのbudget position、totalは
  引き継ぎ分を差し引いた額でした。行を足してもtotalにならず、どちらの数字も
  「これは何の金額か」を名乗っていませんでした。
- `own_cost_usd` / `carried_cost_usd` / `budget_position_usd` / `lineage_cost_usd`
  を分離し、JSONとtable header双方で名乗らせます。`cost_usd`は
  `budget_position_usd`のaliasとして**残します**（documentedでtestも参照しており、
  値自体は変わらないため）。
- `lineage_cost_usd`はancestorがcleanされていれば`null`です。部分和は出しません。
  **lineageの合計行も出しません** — ancestorを共有する行を足すと二重計上になり、
  それはこの分離が解こうとしているbugそのものです。

### failed/stuck runが、実行できないactionを提示していました

- **これは何が起きたか。** failed/stuckには`resume`と`carry_budget`が無条件で
  並んでいました。persistence failureは再開できず、max-visitsで止まったrunは同じ
  flowで再開すればまた止まり、closure/workspaceの問題には先に別のflagが要ります。
  AI callerは`next_actions[].argv`をそのまま実行します。
- 各actionをdiagnosisにしました。`resumable`/`carryable`、`reason`、先に必要な
  `requires`を持ち、**実際に成功し得るときだけ`argv`が入ります**。max-visitsの
  行き止まりはresumeを拒否し、workspace driftや変更されたclosureには該当flagを
  argvへ畳み込みます。

### codexがheadlessでforkできるようになりました

- `codex exec fork`が存在するのに、sfhはcodexのforkをTUI専用として拒否していました。
  branchが欲しいflowは直列に繋ぐか、cold sessionの費用を払うしかありませんでした。
- adapter全体としてはforkを認めたうえで、`build_fork`のcodex armは**installed
  binaryの`--help`にforkが現れることを確認するまで何も組み立てません**。証拠がない
  場合と古い場合は同じく拒否します。既定は拒否で、supportは示される側です。
- **version floorとしては表現していません。** `exec fork`がどのreleaseで入ったかを
  示せる資料がなく、ここに数字を書けばそれは捏造です（grokのflagと同じ誤り）。
  加えて`minimum_version`は報告されるだけで比較には使われないので、floorを置いても
  何もgateしません。`--help` probeは数字を要らず、実際に起動を止めます。

### adapter metadataとrequired_flags

- `required_flags`は手書きで、builderが実際に出すflagから乖離していました。つまり
  preflightの`--help` drift checkは、**列挙されていないflagが消えても気づけません**
  でした。全builder・全access levelを歩いて照合するtestを追加したところ、全adapterで
  不足が見つかったので補いました。
- agyの`minimum_version`を`1.1.8`にpinしました。このpresetのparse pathが依存する
  `--output-format json` envelopeは、agy自身のchangelogでそのreleaseとされています。
  `LAST_VERIFIED`は動かしていません — 実CLIへのlive probeは行っていないためです。

### machine JSON契約とdocumentの訂正

- READMEは`output_file`を「32 MiB超のstreamの全文が残る場所」と説明していました。
  実際にはcanonicalな`.out.txt`自体がbounded captureから書き戻されるので、全文は
  どこにも残りません。structured protocolの最終回答と会計はstream全体から取られる
  ので無事ですが、素の`cmd:` stepにその解釈はなく、記録される出力はまさにその
  上限付きfileです。
- 「すべての`--json` commandが同じenvelopeを返す」は事実ではありませんでした。
  `validate`と`runs list|show|why`はenvelope以前のbare JSONです。どちらがどちらかを
  両側で名指しし、`schema_version`の有無を実行時の見分け方として示しました。
  移行は破壊的変更なので、意図した方向として記録するに留めています。
- error code保証をv1.4.0 tag上で「v1.2.xの間」と書いていた4箇所を、実行時に読める
  `schema_version`基準へ改めました。SECURITY.mdのsupported versionも同様です。
- `docs/machine-api.md`を追加しました。

### `outcomes:` の細かい2点

- Schemaはcanonical decimalしか受け付けないのに、runtimeはtrimしてからparseして
  いました。`" 2 "`や`"02"`はsfhでは動きEditorでは無効、という食い違いです。
  runtimeもcanonical formを要求します。
- preset AI stepに`outcomes:`が付いている場合、`validate --strict`がwarningを出します。
  tableはraw exit codeを読みますが、素のAI CLIはPASSでもREVISEでもexit 0なので、
  `when_label_is`は検証を通ったうえで**永久に発火しません**。errorではなくwarningです
  — 回答を検査してexit statusを決めるwrapperは正当な使い方だからです。

### 非UTF-8 filenameでworkspace fingerprintが失敗していました

- `git ls-files -z`の`-z`は、git自身のC-quotingを**切る**指定です。したがってUnixでは
  filenameが持ち得る任意のbyteが出てきます。lossy decodeしてから組み直したpathは
  diskに存在しないため、`fingerprint`はそこにあるfileでErrを返し、そのworkspaceは
  安全なresumeもcleanupも拒否され続けました。
- 該当callだけraw bytesで扱います。symlink targetも同様で、こちらは結末がより
  厄介でした — 不正byteだけが違う2つのtargetが同じhashになり、**link先の変更が
  「変更なし」としてfingerprintされる**、この関数が絶対にやってはいけないことです。
- filenameがすべて妥当なUTF-8のworkspaceは従来と同じpreimageになるので、既存の
  checkpointは一致し続けます。

### releaseの出所を追えるようにしました

- tagは`v1.4.0`なのに`Cargo.toml`が`1.2.0`のsource archiveが配布され、archiveだけ
  ではそれが分からず、reviewが別のtreeに対して行われました。
- release workflowが`provenance.json`（version / tag / commit / archive sha256 /
  生成時刻）をasset として発行します。tag・Cargo.toml・CHANGELOGの一致checkは従来
  どおり、何かがbuildされる前に落ちます。`docs/distribution.md`に展開後の照合手順を
  足しました。

## v1.4.0 — 2026-08-08

長時間運用のfeedbackから2件。どちらも「sfhが答えを症状として読んでいた」という
同じ形の問題です。新しいkeyを書かなければ既存flowの挙動は変わりません。

### `outcomes:` — exit codeは2つの事実を運んでいる

- **これは何が起きたか。** 「正常に走ったが受け入れ基準はまだ満たしていない」で
  exit 2を返すgateと、クラッシュしてexit 2になったgateを、sfhは区別できません
  でした。前者はstepの失敗として扱われ、`retry_on: transient`の下では**意図的で
  正しく再現性のある答えのために重いテストスイートが再実行**され得ました。
- `outcomes:`はstep自身のexit codeが何を意味するかを宣言します。keyは**生の
  process exit code**です。

  ```yaml
  outcomes:
    2:  {result: continue, label: acceptance_incomplete}
    10: {result: retryable}
    20: {result: fail}
  ```
- `result`の語彙は意図的に小さく、ドメイン非依存です。`complete`（仕事は終わった）、
  `continue`（役目は果たした、まだ続きがある — **失敗ではない**ので`on_error`は
  発火せずretryも検討されない）、`retryable`（textが何と言おうと再試行に値する）、
  `fail`（最終的な失敗、`transient`でも再試行しない）。sfhが学ぶのは「続けるか、
  再試行するか、止めるか」だけです。
- ドメインの形をしたものはすべて`label`に入ります。sfhはそれを保存し、
  `{{steps.<id>.label}}`で公開し、`when_label_is:`でルーティングし、`step_end`に
  記録し、**決して解釈しません**。sfhは「acceptance」が何かを知りません。
- `when_label_is:` / `when_outcome_is:`をrouteに追加しました。前者は「判定を散文
  から読み取る」ことの決定論的な代替です。`when_last_line_is: PASS`はmodelが最終行を
  ちょうどそのtokenで終えることに依存しており、一言添えられただけで書式の理由で
  stuckになります。
- 保証3つ: entryの無いexit codeは**従来どおりの読みを保つ**／宣言されたoutcomeは
  retryの推測に**上書きする**（足さない）／**protocolの証拠は依然として優先する**
  （structured protocolが完了しなかったturnを受け入れる免許ではなく、timeout・
  中断されたstepでは参照されない）。
- 決して一致し得ないrule（どのentryも持たないlabel、どのentryも宣言していない
  outcome class）、`0: {result: retryable}`、空のlabel、負のexit codeは
  `validate`のエラーです。3時間走ったあとの驚きではありません。

### `retry_on: transient` がコマンドのstdoutを症状として読まなくなりました

- **これは何が起きたか。** transient判定のneedleは`429` `502` `rate limit`
  `connection reset`など22個で、すべて**provider障害**を指す語です。preset stepなら
  tool自身の報告なので正しい。しかし`cmd:` stepのstdoutは**結果データ**です。
  検証スイートに`tcp_502_returns_error`というテストが含まれているだけで、
  決定論的な失敗のたびにスイート全体が再実行されていました。テスト名の中の`502`
  への一致で、数分の計算と2回目の請求が発生します。
- `cmd:` stepのstdoutは走査しなくなりました。stderrは引き続き見ます。プログラムが
  運用上の問題を報告するのはそこで、`curl`の"connection reset by peer"はまさに
  transientそのものだからです。preset stepは変更なしです（chain outputはtool自身の
  報告なので）。

## v1.3.0 — 2026-08-08

v1.2.1のすべての修正に加えて、そのv1.2.1のきっかけになった事例が残していた
最後の穴を塞ぎます。新しいflow keyはありません。既存flowの挙動も変わりません。

### `--carry-budget-from`: flowを直したあとも、使った予算を持ち越せるようになりました

- **これは何が起きたか。** runが止まり、原因が「flow自体が間違っていた」だった
  とき、flowを直すのが正しい対応です。ところが直すとeffective-config
  fingerprintとexecution closureが変わるので、`--resume`は正しく拒否します。
  残された手段は「新しいrunを始める」だけで、そのrunのcounterはすべてゼロから
  始まります。つまり**すでに使った予算が消えます**。実際の運用では、flowの上限を
  手で書き換えて（「1回消費済みなので残り9回」）辻褄を合わせることになりました。
  手計算は会計ではありません。検証できず、数え間違えた瞬間に壊れ、2回目のrunが
  1回目の続きだったという記録もどこにも残りません。
- `sfh run <flow> --carry-budget-from <run-dir>` は**新しいrun**を開始し、
  そのrunに先行runの支出を引き継がせます:
  - `max_total_steps` に対するleaf run数
  - `max_visits` に対する**step単位の訪問回数の最大値**（loopの残り回数がそのまま
    残り回数として効きます）
  - `max_cost_usd` に対する報告済みcost
  - `wall_clock_sec` に対するactive時間
- **引き継ぐのはcounterだけです。** step outputもsessionもrouting位置もworkspaceも
  引き継ぎません。それらを作ったflowは、これから走るflowではないからです。
  `--resume`が拒否した理由がまさにそれです。両者は別の問いなので、同時指定は
  usage errorにしています。
- **合成します。** 引き継いだrunからさらに引き継ぐと、最初のrunの支出も残ります。
  2回目の修正で1回目の支出が黙って消えるのは、この機能が手作業から取り上げようと
  しているまさにその算術なので、`budget_carried` eventはlog読み取り時に
  baselineとして畳み込まれます。
- **記録が残ります。** `budget_carried` durable event、`meta.json`の
  `carried_budget`、そして人間向けの1行。corrected flowがもう定義していないstep id
  は「適用できなかった」と**名指しで**報告します（黙って忘れません）。
- 止まったrunのJSON envelopeは `resume` と `carry_budget` の**両方**をnext actionと
  して出します。flowが悪かったのか世界が悪かったのかを知っているのは読み手だけ
  だからです。
- **二重計上しません。** 引き継いだrunの `cost_usd` は先行runの支出を含みます
  （`max_cost_usd` はその値で判定されるので当然です）が、先行run自身の行にも同じ
  金額が載っています。`sfh runs list` の `total_cost_usd` は各runの
  `carried_cost_usd` を差し引いて合計するので、hopを重ねても実際に払った額のまま
  です。`sfh runs show` は引き継いだ分を1行で明示します。
- 引き継いだactive時間も `budget_carried` eventから復元できます。`status.json` は
  detachした`--resume`で作り直されるため、そこだけを頼りにすると
  `wall_clock_sec` の引き継ぎだけが静かにゼロに戻っていました。

## v1.2.1 — 2026-08-08

v1.2.0を実運用へ投入して見つかった4件の穴を塞ぐrelease。新しいkeyを書かなければ
既存flowの挙動は変わりません。`api_version: 1`も維持します。

### exit codeとprotocol evidenceの衝突を、flowが宣言できるようにしました

- **これは何が起きたか。** 最終回答を完成させ、cleanなcommitまで済ませたAI CLIが、
  途中のtool callが1つ失敗していたためprocess exit=1を返しました。sfhはterminal
  recordを受け取って成功をcertifyできていたのに、exit codeを理由にstepを落とし、
  runがstuckで止まりました。回避手段が「flowからexit code判定を外す」しかなく、
  それはfail-openです。
- `exit_conflict: fail | trust_protocol`をstepと`defaults`に追加しました。
  `trust_protocol`は、`certifies_success()`が真のとき — つまり文書化されたterminal
  recordが存在し、壊れておらず、成功と述べているとき — に限りexit codeを覆します。
  raw text、未知のstatus、壊れたenvelope、terminal欠落はこの条件を満たさないので、
  stdoutへ出したusage errorが成功stepになることはありません。
- 既定は全adapterで`fail`のままです。v1.2.0が唯一例外にしていたagyは、`matches!`の
  ハードコードではなく`AdapterInfo::exit_code_trustworthy`というdataになりました。
  挙動は同一です。
- `trust_protocol`を使わない場合でも、**衝突が起きたことをsfhが黙らなくなりました。**
  「exit Nだったが、tool自身のprotocolはこのturnを成功としてcertifyしている」旨と、
  `exit_conflict:`という正しい対処、そして「exit code判定自体をやめるな」という但し
  書きを、stepのstderr・error artifact・`sfh runs why`へ載せます。
- `trust_protocol`は`sfh plan --json`の`unsafe_overrides`に出ます。

### preflightが`cmd:`stepのprogramを見るようになりました

- **これは何が起きたか。** Windowsで検証stepが`bash`と書かれており、PATH上先頭の
  `%SystemRoot%\System32\bash.exe`（WSL launcher）へ解決していました。WSLは別OSなので
  Windowsのworktreeも`.git` gitfileも読めず、全検証が5秒で落ちました。それでも
  `sfh preflight`は「no blockers」と答えていました — 検査して通ったのではなく、
  `resolved_tools()`が`cmd:`stepを対象外にしていたからです。
- preflightが`cmd:`stepのprogramも解決し、絶対pathとそれを起動するstep idを報告する
  ようになりました（`--json`では`commands`）。解決できないprogramはblockerです。
- **解決するだけで、実行はしません。** `--help`/`--version`をadapterへ送るのは安全でも、
  flowが名指しした任意のprogramへ送るのは安全ではありません（`deploy.sh --help`は
  deployし得ます）。実際に問題だった問い — 「この名前はどのbinaryか」 — は解決だけで
  答えられます。
- bareな`bash`/`wsl`がWindowsのSystem32/Sysnative/SysWOW64へ解決した場合はblockerに
  し、Git for Windows bashのpathを示します。PATHが選んだ場合のみで、flowが明示的に
  full pathを書いた場合は何も言いません。
- `cmd:`をstringで書いたstepは、flowが選んだshellではなくplatform shell（`sh`/`cmd`）
  で走ることも報告します。

### context bodyがsfhのdelimiterを偽装できた問題を塞ぎました

- v1.2.0はcontextの**name**をescapeし、**body**をrawのまま埋めていました。bodyは
  fileの中身かtemplateの描画結果であり、templateは前のstepのoutputを展開できます。
  つまりmodelが書いたtextがbodyに入り得ました。`</sfh-context>`を含むbodyは自分の
  blockを早期に閉じ、`<sfh-prompt>`を含むbodyは「ここからが実際の指示だ」とsfhが
  宣言するsectionを偽造できました。
- bundleのbodyと、prependされるprompt本体の両方で、4つのdelimiter tokenの先頭`<`を
  `&lt;`にします。触るのはこの4 tokenだけで、他の文字・記号・codeは1 byteも変わりま
  せん。何も削除しません。

### `{{context_file}}` / `{{context}}` が validate を通るようになりました

- v1.2.0は`context_delivery: file`を「promptから`{{context_file}}`を指せ」と文書化
  しながら、自身のtemplate precheckが`context`と`context_file`をunknown keyとして
  拒否していました。文書どおりのflowが`sfh validate`で落ち、runにも到達しません
  でした。runtimeは両方を常に定義しています（contextを持たないstepでは空文字列）。
  precheckをruntimeに揃えました。

### preflightが報告するpathが、実際に起動されるpathと一致するようになりました

- `which()`はWindowsで拡張子なしの候補も返していましたが、実行側はそれを起動しません
  （`.exe`/`.cmd`/`.bat`のみ）。Unixではexec bitを見ていなかったため、`execvp`が読み
  飛ばすfileを「これが起動される」と報告し得ました。両方を実行側の規則に揃えました。

## v1.2.0 — 2026-08-07

sfhを「仕様が変わり得る外部CLI・AI CLIを、宣言された制御フロー、作業環境、入力
文脈、権限、証拠、再開規則のもとで実行する汎用durable harness」として定義し直し、
その定義に足りていなかった実行基盤を追加したreleaseです。

新しいkeyを一つも書かなければ、既存flowの挙動は変わりません。同じcwd、同じruns
root、同じstdout、同じroute、同じresume semanticsです。`api_version: 1`も維持します。

### 構造化protocolのfail-closed（意図した厳格化）

- preset toolは、そのCLIが文書化しているmachine-readable protocolを最後まで完了
  しなければならなくなりました。terminal recordが届かなかったstreamや、文書化され
  た形ではないstdoutは、`text`として下流へ渡されずstepの失敗になります。
  新設の`src/protocol.rs`が`ProtocolState`（plain/valid/missing_terminal/invalid）と、
  terminal recordの有無・verdict・final message・malformed record数をevidenceとして
  保持し、実行層はtextではなくこのevidenceから判断します。
- **agyの偽の成功を修正しました。** 非ゼロexitを成功へ補正する経路が「textが空で
  なく、失敗と明示されていない」だけで発火し、agyのparserはenvelopeを解釈できない
  ときrawなstdoutをそのtextとして返していました。この2つが噛み合うと、usage errorを
  stdoutへ出してexit 1したinvocationが、usage messageを回答とする成功stepになり得ま
  した。非ゼロexitを0へ補正できるのは、adapterが認識した正規のterminal success record
  がある場合だけになりました。raw text、未知のstatus、壊れたenvelope、terminal欠落の
  いずれもこの条件を満たしません。補正の適用範囲も、exit codeが信頼できないと文書化
  されている唯一のadapterであるagyに限定しました。
- 7つのpreset parser全てがterminal recordを要求します。protocolが完了しなかった理由は
  sfh自身が生成したbounded diagnosticとしてstderr、stepのerror artifact、`step_end`、
  `sfh runs why`に載ります。custom `cmd:`は`ProtocolState::Plain`で、従来のstdout契約の
  ままです。
- **1.1でraw textのまま成功していたrunは、1.2では失敗します。** これはbug fixであり、
  互換維持の対象外です。

### 権限configとpromptの漏洩

- opencodeのpermission configを`format!`ではなく`serde_json`で構築するようにしました。
  agent名はflow dataなので、quote・backslash・control characterを含み得ます。文字列
  連結では、opencodeが破棄する不正なJSON（=deny ruleが黙って消える）か、攻撃者が選んだ
  構造のどちらかを生み出せました。
- argvでpromptを渡すadapterでは、そのargv要素をdurable logへ書かなくなりました。
  子processには実物が渡り、記録には`<prompt chars=N sha256=...>`が残ります。binary
  path、flag、model、access、cwdなど診断に必要な情報はそのままです。

### AI向けmachine interface

- `run` / `plan` / `wait` / `stop` / `status` / `preflight` / `workspaces` に`--json`。
  共通envelope（`schema_version` / `command` / `ok` / `state` / `terminal` / `exit_code` /
  `run_id` / `run_dir` / `error` / `warnings` / `next_actions`）を返します。
- JSONモードのstdoutはenvelopeだけです。進捗・warning・plan headerはstderrへ、結果は
  envelope内の`result` / `result_file`へ移しました。設定エラーでもprose ではなくenvelope
  を返します。ここはmachine callerがparseできないと一番困る場面だからです。
- 失敗には、v1.2.x内で意味が変わらないcodeが付きます: `SFH_USAGE` / `SFH_FLOW_INVALID` /
  `SFH_PROTOCOL_INVALID` / `SFH_TERMINAL_MISSING` / `SFH_SESSION_UNVERIFIED` /
  `SFH_EXECUTION_CLOSURE_CHANGED` / `SFH_WORKSPACE_MISSING` / `SFH_WORKSPACE_DRIFT` /
  `SFH_WORKSPACE_BUSY` / `SFH_WORKSPACE_UNOWNED` / `SFH_REPLAY_REFUSED` /
  `SFH_PERSISTENCE_FAILURE` / `SFH_CAPABILITY_UNAVAILABLE`。
- detached runは`"terminal": false`のhandleと、答えを待つargvを返します。path省略で
  最新runを選んだ場合は`"implicit_target": true`を必ず返します。
- `status --json`は追加fieldのみで、従来のfieldと意味は変わりません。

### preflight（無料の事前確認）

- `sfh preflight [flow.yaml] [--profiles f] [--state-dir d] [--json]`を追加しました。
  model呼び出しを一切行わず、flowが実際に起動するtool/bin variantだけを調べます:
  binaryの所在とversion、adapterが依存するflagがそのCLIの`--help`に残っているか、
  protocol、resume/fork対応、cost coverage、access levelごとの強制度
  （sandboxed/enforced/best-effort/unsupported）、既知のgap、そしてこのflowが作る
  workspace・contextとstatic leaf上限です。
- adapterの`minimum_version`はどれも設定していません。各CLIの公式文書とlive probeで
  確認していない下限を主張する代わりに、preflightはインストール済みversionを表示し、
  要件は不明であると述べます。
- `doctor`は従来どおりreal callを行いますが、隔離したscratch directoryから実行される
  ようになりました。実行した場所のinstruction fileを読まず、そこへ書き込むこともあり
  ません。外部CLIへ渡すpathは全てabsoluteになりました。

### managed workspace

- `workspace:`（`mode: current|directory|git-worktree|auto`、`root` / `base` /
  `cleanup` / `allow_concurrent_writers` / `verify_on_resume`）と、stepごとの
  `effects: read|workspace|external|unknown`を追加しました。
- `auto`は仕事の意味を推測せず、宣言された`effects`だけから決めます。全stepがreadなら
  workspaceを作りません。書き得るstepが1つでもあれば、run全体で**1個**のgit worktreeを
  作ります。step数にもvisit数にも依存しません。
- worktreeはbranch元repositoryの外側（`--state-dir`配下、またはplatformのuser-state
  directory）に`sfh/<flow>/<run-id>` branchで作られます。呼び出し元のcheckoutは変更
  されません。
- **sfhは自分が作ったpathしか削除しません。** ownership markerとrun manifestのnonceが
  一致した場合だけで、しかも削除直前に再確認します。**未コミットの変更を自動的に破棄
  することはありません。** dirtyなworkspaceはrunの結果に関わらず保持され、branchも削除
  されません。failed/stuck/stopped/deadのrunは常にworkspaceを残します。
- resumeではworkspaceのfingerprint（HEAD、index差分、working tree差分、untracked file
  全件のhash、submodule状態）を最後のdurable checkpointと比較します。未完了stepで説明
  できない差分は拒否し、`--adopt-workspace`で明示的に採用できます。`--force-resume`とは
  別の問いで、片方がもう片方を免除しません。
- `sfh workspaces list|show|clean|remove`を追加しました。`remove --discard`が、sfhで
  未コミットの変更が失われ得る唯一の経路です。

### named context

- top-levelの`contexts:`（`file` / `inline` / `template`、`max_chars` /
  `allow_external` / `optional`）と、stepの`context:` / `context_delivery:`を追加。
- 決定的な順序と区切りでbundleを組み立て、`<tag>.context.txt`と、各sourceの出所・hash・
  サイズを記録した`<tag>.context.json`を保存します。durable logにはhashだけが載ります。
- context fileはno-followで読まれ、flow directoryまたはworkspaceの内側に解決される必要
  があります。外を指すsymlinkは拒否され、`allow_external: true`が唯一の逃げ道です。
- `defaults.max_context_chars`超過は**何も起動する前に**失敗します。sfhは要約もしませんし、
  収まるようにsourceを落とすこともしません。
- `{{context}}`と`{{context_file}}`をbuiltinとして公開しました。

### execution closure

- flow本体、実効config、profile overlay、context fileの中身、tool version、workspaceの
  modeとbase commit、unsafe overrideの集合をcanonical JSONのSHA-256で固定し、
  `execution-closure.json`と`meta.json`へ記録します。
- resume時に差があれば既定で拒否し、動いたentryを名指しします。`--force-resume`で明示的に
  受け入れると`force_resume` eventが残ります。
- fileはpathではなく**中身**で固定し、CRLFはLFへ畳みます。同じflowを別のpathや別の
  checkoutからresumeしても同一と判定されます。flow fingerprintが元々採っていた方針と
  同じです。

### 再利用可能なflow: profile overlay

- `--profiles <file>`（繰り返し可、後勝ち）を`run` / `plan` / `validate` / `preflight` /
  `config show`に追加しました。共有flowが`use: judge`とだけ書き、実行する人がtool・model・
  binを外から決められます。
- overlayは書かれたfieldだけを置き換えます。`args`は指定があれば置換・なければ維持、
  `env`はkey単位でmerge。優先順位は step field > overlay > flow inline profile >
  `~/.sfh/profiles.yaml` > defaults。stepに直接`tool:`を書く従来の書き方はそのままです。

### replay policy

- `defaults.replay.unfinished`とstepごとの`replay.unfinished`（`rerun|stuck|fail`）を
  追加しました。開始されたのに終了を記録しなかったstepを、resumeがどう扱うかの宣言です。
- 既定は従来と同じ`rerun`です。`stuck`（exit 4）と`fail`（exit 1）は何も起動せず、
  workspaceと部分成果物を残して`SFH_REPLAY_REFUSED`を返します。
- `effects: external|unknown`かつ`rerun`のstepは`validate --strict`と`plan`がwarningを
  出します。retry・fallback・visit・完了済みstepの再利用とは別物です。

### state root

- `--state-dir <dir>` / `SFH_STATE_DIR`を追加し、`runs` / `workspaces` / `plans` /
  `doctor`を1つの根の下に置けるようにしました。
- `--runs-dir`は従来どおりrun artifactsだけを移し、どちらも指定しなければrunは今までどおり
  `.sfh/runs`に落ちます。state rootのないmanaged workspaceはplatformのuser-state directory
  （`$XDG_STATE_HOME/sfh`、`$HOME/.local/state/sfh`、`%LOCALAPPDATA%\sfh`）へfallbackし、
  それも決められない場合はrepository内へ黙って書く代わりにerrorになります。

### 証拠とdiagnostics

- `step_start`に`protocol_expected` / `context_hash` / `context_file` / `workspace_id`、
  `step_end`に`protocol_state` / `terminal_seen` / `terminal_success` /
  `final_message_seen` / `malformed_records`を追加しました。いずれも追加のみで、既存
  eventの意味と型は変わりません。
- 新event: `execution_closure` / `workspace_created` / `workspace_checkpoint` /
  `workspace_adopted` / `workspace_cleanup` / `force_resume`。
- `sfh runs why --json`が`protocol_failure`を構造化して返します。
- `plan --json`がworkspace plan、context plan、execution closure、replay policy、
  unsafe override、static leaf上限、redactedなinvocationを返します。
  `plan --save [dir]`で、renderされたprompt・context bundle・machine planを保存して
  実行前に確認できます。

### 互換性と移行

- 新しいkeyを使わない既存flowは、cwd・runs root・stdout・route・resume semanticsとも
  従来どおりです。`api_version: 1`のままです。
- v0.x / v1.0 / v1.1のrun fixtureは従来どおり読めます。新fieldのない古いlogでも
  `status` / `show` / `why`は動きます。
- `--force-resume`の既存のaccess fail-closed挙動は弱めていません。
- 唯一意図的に壊した挙動は、上記のprotocol fail-closedです。

### 非scope

subflow、writerごとのworkspace自動fork、自動merge/PR/conflict解決、named resource
semaphore、first-class secret provider、`access`のcapability lattice置換、typed JSON
route、`await:`、replay `probe`、container/remote workspace、自動context要約、
background GC、frozen `run --plan`、native session rollover、role予約語は
v1.2.0のscope外です。今回のdata modelは、これらを後付けできる中立的な名前と構造に
しています。

## v1.1.5 — 2026-08-07

### 構造化streamの完全性

- Piの`--mode json`をpipe reader上で逐次解析し、raw transcriptが32 MiBを超えても
  末尾のassistant最終回答、session marker、全assistant messageのtoken・costを失わないようにした。
  transport logの大半を占める`message_update`がcapture上限へ達しても、正常なReviewerを
  「final messageなし」と誤判定せず、予算を過少計上しない。
- raw stdout/stderr artifactは無制限にdiskへ書かず、32 MiBを超えた場合に先頭16 MiBと
  末尾16 MiBを省略marker付きで保持する方式へ変更した。session headerとterminal event/errorを
  同時に調査でき、途中経過だけを残して末尾を捨てる旧挙動を廃止した。
- 1件のPi JSONL recordが16 MiBを超えて安全に逐次解析できない場合は、最終回答・会計を
  推測せずstepをfail-closedにする。sfh自身が生成したbounded診断を`step_end`へ記録し、
  `sfh runs why`から具体的な停止理由を確認できるようにした。

### 回帰検証とドキュメント

- 34 MiB超の有効なPi streamを3 OSのengine suiteで実行し、末尾verdict、前後のusage/cost、
  head+tail artifactをproduction経路で検証するstandalone stubを追加した。raw captureとsemantic
  observerの単体境界、16 MiB単一recordのfail-closed診断も回帰テストに含めた。
- 長いcommand出力をAI promptへ渡す際は`tail`/`truncate` filterで明示的に制限し、全文は
  `output_file`から読む運用を日英READMEへ追記した。
- 英語版を標準の`README.md`、日本語版を`README.ja.md`へ整理した。
  初見利用者向けの導入・最小フロー・主要概念に絞り、詳細仕様はCLIヘルプ、
  `sfh guide`、examples、公開Schemaへ段階的に案内する構成へ短縮した。

## v1.1.4 — 2026-07-30

### 1コマンド導入

- macOS / Linux向け`sfh-installer.sh`とWindows向け`sfh-installer.ps1`を
  Release assetとして追加した。OS・CPUと最新assetを自動選択し、同梱SHA-256を
  照合してからユーザー領域へatomicに導入する。既定ではPATHも設定し、再実行による
  更新、`SFH_VERSION`でのversion固定、`SFH_INSTALL_DIR`、
  `SFH_NO_MODIFY_PATH`にも対応する。
- Windows installerは検証済みファイルのMark-of-the-Webを解除し、arm64環境では
  Windowsのx64互換実行を明示して既存x64 buildを選ぶ。Unix installerは
  Linux/macOSのx64/arm64を判定し、bash/zsh/fishを含むPATH永続化を扱う。
- 日英READMEの入口を公式ワンライナーとHomebrewへ整理した。手動・
  オフライン用のbinary/checksumとGit source buildは代替経路として維持する。

### パッケージマネージャとリリース契約

- 4つのUnix platformのRelease checksumを唯一の入力として、Homebrew Formulaを
  生成する決定的なrendererを追加した。version、download URL、hashの手書き転記をなくした。
- Release workflowはbinary build完了後にinstaller、Formulaと
  各checksumを生成し、binaryと同じGitHub Releaseへまとめて公開する。
- CIは3 OSで公式installerを実際のrelease形式に対して実行し、PATHから起動できることと、
  1 byteでも改変されたarchiveをSHA-256不一致として拒否することを検証する。
  FormulaのRuby構文、欠損・不正checksum、version path traversalも検査する。

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
