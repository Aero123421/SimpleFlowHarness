# SFH authoring skills

**SFH v2.0 のフローを設計・レビューする AI 向け**の Skills です。実行先の AI に渡す Skills とは別です。YAML に `skills:` キーはなく、実行時の指示には `contexts:` または CLI 固有の設定を使います。SFH 自身は native Skill の読込を証明しません。

新規設計は `sfh-flow-design`、既存フローの監査は `sfh-flow-review` が入口です。専門 Skill は必要な問題に応じて選びます。

| Skill | 扱う問題 |
|---|---|
| `sfh-loop-engineering` | フィードバックループの進捗と停止 |
| `sfh-deterministic-gates` | 証拠から route を選ぶ方法 |
| `sfh-context-workspace` | ファイル・入力・session の共有 |
| `sfh-failure-recovery` | 安全に再試行・再開できる範囲 |
| `sfh-ci-monitoring` | 正しい CI run の特定と監視 |
| `sfh-tool-integration` | 外部 CLI/API/MCP の接続契約 |
| `sfh-eval-engineering` | 失敗記録から回帰検査を作る方法 |

参照資料やサンプルは必要時に使います。全 Skill の一括読込は不要です。SFH は worker → check → evaluator の閉ループを宣言 route で構成できます。品質を判断するのは check/evaluator であり、エンジンではありません。

配布物のルートから project scope に配置する例:

```bash
mkdir -p .agents/skills
cp -R skills/sfh-* .agents/skills/
```

クライアント別の配置は [INSTALL.md](INSTALL.md) を参照してください。作成したフローは installed guide/schema、`sfh validate FLOW --strict`、`sfh run FLOW --dry-run` で確認します。live 実行前の preflight と plan はローカル能力・実行内容の確認であり、認証成功や結果不明の副作用の安全な再実行を保証しません。

配布検査: `python3 tests/skills_checks.py`、`python3 skills/tools/validate_pack.py skills`。選択と作成品質の検証は [EVALS.md](EVALS.md)、設計根拠は [SOURCES.md](SOURCES.md) を参照してください。
