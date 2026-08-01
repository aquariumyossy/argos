# Argos

ドラッグした文字列を、ローカル知識ベースから一瞬で検索する Windows デスクトップアプリ。

FullTextSearchApp の後継として開発中。

## 開発

前提: Node.js、Rust、Visual Studio Build Tools（MSVC）

```bash
npm install
npm run tauri dev
```

初回ビルドは Lindera IPADIC 埋め込みのため時間がかかることがあります。

## 配布パッケージ（v1.0）

Windows 向け NSIS インストーラ（管理者権限不要）を作成します。

```bash
npm run package
```

成果物:
- `release/Argos-v1.0.3/Argos-Setup-v1.0.3.exe` — ダブルクリックでインストール
- `release/Argos-v1.0.3-windows-x64.zip` — 上記一式の ZIP

## 使い方

1. トレイアイコンから「設定を開く」
2. 検索対象フォルダを追加
3. 「今すぐインデックス」を実行
4. 任意アプリで文字列を選択し `Ctrl+Alt+A`
5. ポップアップで結果確認 — `Enter` で開く / `Ctrl+Enter` でプレビュー / `Esc` で閉じる

## LAN リモート検索

別 PC で動いている Argos の索引を、同じ LAN 上の別の Argos から検索できます（Elasticsearch は不要）。

### ホスト側（索引がある PC）

1. 検索対象フォルダを登録しインデックスする
2. 設定の「リモート」タブで「リモート検索サーバを有効にする」をオン
3. 表示された共有トークンを控える（クライアントに同じ値を入れる）
4. Windows ファイアウォールでポート（既定 `17890`）の受信を許可する

### クライアント側

1. 「リモート」タブで検索モードを「リモートのみ」または「ハイブリッド」に設定
2. リモート URL（例: `http://192.168.x.x:17890`）とホストと同じトークンを入力
3. 「接続テスト」で確認してから保存

リモート結果のパスがホストのローカルドライブ（`C:\...` など）の場合、クライアントからはファイルを開けないことがあります。プレビューは利用可能です。両方から開けるようにするには、ホスト側の設定でフォルダに「公開パス（UNC）」（例: `\\HostName\Share`）を設定してから再インデックスしてください。ネットワークドライブ（`Z:\` など）を追加した場合は、可能な限り UNC が自動設定されます。

## 技術

- Tauri 2 + React + TypeScript
- Rust / SQLite / Tantivy / Lindera（IPADIC）
- ファイル監視: notify
- LAN 検索 API: axum（ホスト） / reqwest（クライアント）

データ保存先: `%APPDATA%\Argos\`
