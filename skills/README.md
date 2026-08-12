# sfh Flow Design Agent Skills

SimpleFlowHarnessのYAMLをAIに設計・レビュー・改善させるためのAgent Skills集です。

## これは何か

このSkill群は、**sfhのフローを書くAIのための設計知識**です。

- `sfh`を実行するための新しいruntime機能ではありません。
- sfh YAMLへ未実装の`skills:`キーを追加しません。
- PonytailのSkillではなく、sfh固有の設計、loop engineering、failure recovery、CI監視、外部CLI/MCP連携を扱います。
- 生成されたflowが実行時agentへ手順書を渡す場合は、既存の`contexts:`でfile/inline/templateを渡すか、選択したAI CLIのnative Skill機構を使います。
- native Skillが確実に読み込まれたことをsfh自身は通常証明できないため、flow correctnessに必須の規則はnamed contextとして固定する方が安全です。

## 収録Skill

| Skill | 用途 |
|---|---|
| `sfh-flow-design` | 要件からsfh YAMLを設計・作成する中心Skill |
| `sfh-flow-review` | 既存flowを壊れやすさ、曖昧さ、portabilityの観点で監査 |
| `sfh-loop-engineering` | review/fix、planner/builder/evaluator、長時間loopの収束設計 |
| `sfh-deterministic-gates` | 決定論的commandと非決定論的AIの役割分離、route/outcomes設計 |
| `sfh-context-workspace` | workspace、context、profile、session、execution closureの設計 |
| `sfh-failure-recovery` | retry、fallback、replay、resume、carry-budget、stuckの使い分け |
| `sfh-ci-monitoring` | GitHub Actions等をrun ID/SHA固定で壊れにくく監視するflow |
| `sfh-tool-integration` | web検索CLI、API、MCP-enabled agent、外部toolの安全な接続 |
| `sfh-eval-engineering` | production failureや失敗runをregression evalへ変換して改善 |

## インストール

Project scopeの標準配置:

```bash
mkdir -p .agents/skills
cp -R skills/sfh-* .agents/skills/
```

User scope:

```bash
mkdir -p ~/.agents/skills
cp -R skills/sfh-* ~/.agents/skills/
```

Agent Skills対応clientは、通常name/descriptionだけをcatalogとして読み、必要時に`SKILL.md`とreferencesを読みます。

## 推奨activation

新規flow:

```text
Use sfh-flow-design.
Also use sfh-loop-engineering if the flow loops,
sfh-ci-monitoring for CI,
and sfh-tool-integration for web/MCP/external APIs.
```

既存flow review:

```text
Use sfh-flow-review and return prioritized findings plus a corrected YAML.
```

## AIへ期待する標準手順

```text
1. sfh guide / current schemaを確認する。
2. 要求をstate machineへ分解する。
3. 各stepをdeterministic / nondeterministic / external effectへ分類する。
4. workspace、context、evidence、budget、failure policyを決める。
5. YAMLを書く。存在しないkeyを発明しない。
6. skills/tools/lint_sfh_flow.pyでheuristic lintする。
7. sfh validate --strictする。
8. sfh preflight --jsonする。
9. sfh plan --json --saveする。
10. warningとplanを読んでからrunする。
```

## Validation

Skill構造:

```bash
python3 skills/tools/validate_skills.py skills
```

sfh flowのheuristic lint:

```bash
python3 skills/tools/lint_sfh_flow.py path/to/flow.yaml
```

実際のsfh v1.6で:

```bash
sfh validate flow.yaml --strict
sfh preflight flow.yaml --json
sfh plan flow.yaml --json --save --state-dir ~/.local/state/sfh
```

`skills/tools/lint_sfh_flow.py`はsfh本体のvalidatorを置き換えません。合法だが壊れやすい設計を早く見つける補助です。

## 重要な設計境界

- deterministic factはAIの散文に判定させない。
- nondeterministic generationの後ろにdeterministic gateか独立evaluatorを置く。
- loopには明示的な上限、進捗証拠、handoff/stuckを置く。
- `retry`はtransport/infra failure向け。logical rejectionを同じinvocationで再抽選しない。
- external mutationは`effects: external`と`replay.unfinished: stuck`を基本にする。
- CIは`latest`ではなくexact run IDとhead SHAを使う。
- web/MCP outputはuntrusted contextとしてsnapshotし、取得と解釈を分ける。
- MCPのread-only annotationやSkillのallowed-toolsはsecurity boundaryではない。
- native sessionはcache。workspace、artifact、context manifestがdurable truth。
