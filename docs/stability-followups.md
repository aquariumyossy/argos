# 安定性まわり — 残課題メモ

最終更新: 2026-08-10  
前提: 安定性監査レポートのフェーズ 1〜4 は実装済み。以下は **緊急性は高くない** フォローアップ。

---

## 既知のトレードオフ（仕様として許容）

### STA タイムアウト後も裏でジョブが続く
- **内容**: Outlook STA の呼び出し側は `recv_timeout` でエラー返却するが、COM 処理自体はキャンセルされない。
- **影響**: UI は固まらない。直後のメール操作は、前ジョブ完了までキュー待ちになり得る。
- **改善案（任意）**: ジョブキャンセル／世代 ID、または「同期中は他操作を拒否」の明示 UI。

### 再索引中の FS イベントを捨てる
- **内容**: `reindex_busy` 中は watcher の `index_path` / `remove_path` をスキップ（キューしない）。
- **影響**: 再索引中の追加・削除が稀に取り残される。次の変更イベントや手動再索引で収束。
- **改善案（任意）**: 再索引完了後に短い追従スキャン、または busy 中イベントの簡易バッファ。

### `AppState::open` 失敗時は起動不可（C1 の残り）
- **内容**: トレイ／ショートカット失敗は継続起動だが、DB・索引オープン失敗は setup 失敗のまま。
- **影響**: データ破損時はプロセスが立ち上がらない（従来同様）。頻度は低い。
- **改善案（任意）**: エラーダイアログ、壊れた index の退避＋空で起動。

### トレイ構築失敗時はトレイ無しで起動
- **内容**: `setup_tray` 失敗でもアプリは続く。
- **影響**: トレイから設定を開けない。ショートカット／単一インスタンス起動に依存。

---

## 未着手（低優先）

| ID | 内容 | 影響 | メモ |
|----|------|------|------|
| I5 | Settings の `busyFolderId` が progress 遅延で sticky | フォルダ行が「処理中」のまま残る可能性 | `finally` 後の遅延 progress 対策、または完了イベント |
| I6 | `listen().then(unlisten=)` の cleanup レース | StrictMode 等で二重リスナ | ready/focus 系は改善済み。他リスナは旧パターン残 |
| I3 | 重い `search_query` 等が同期 IPC | 大きな索引で固まり感 | `async` + `spawn_blocking` 化 |
| — | ショートカット正規化の FE/BE 不一致 | FE は重複を黙って差し替え、BE は Err | ポリシー統一 |
| — | Popup `parentDir` vs バックエンド `pathutil` | UNC / `\\?\` でスコープ親がズレうる | 親パス計算を BE に寄せる |
| — | `search_mode=remote` でもローカル mail をマージ | 「リモートのみ」の期待と矛盾し得る | **仕様確認が必要**（勝手に変えない） |
| — | `is_app_ready` コマンドが FE 未使用 | 害なし | retry / `argos-ready` でカバー済み。整理してよい |

---

## 触らない方がよいもの（当面）

- Tantivy スキーマ変更
- Outlook COM 本体の大規模書き換え
- 検索スコアリングの変更

---

## 関連実装（対応済みの参照）

- Settings / Notes: 起動リトライ、`argos-ready`、focus 再読込
- `trigger_search`: 失敗時も `searching: false`
- STA: `recv_timeout`、メールコマンドの `spawn_blocking`
- `set_mail_last_sync_now`: 単一キー UPSERT
- Indexer: `reindex_busy` 排他
- Remote: port/token 同一時は再 bind しない
- Popup: `ensure_popup_window`、ハイライトは `highlightText.tsx` 共有
