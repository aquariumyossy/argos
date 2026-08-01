# Argos

手元の文書を、いつでも一瞬で全文検索できる Windows デスクトップアプリです。

ブラウザや Word で気になった文言をドラッグ（または選択）してショートカットを押すだけ。登録フォルダ内の PDF・Word・一太郎・Excel・テキストなどを横断し、該当箇所へたどり着けます。クラウドに送らず、すべてローカルで動きます。

- **いつでも全文検索** — トレイ常駐。選択文字列からの起動にも対応
- **軽量・高速** — デスクトップ向けに絞った索引で、ポップアップにすぐ結果表示
- **形態素解析による広いマッチ** — 日本語を単語に分解して検索するため、表記の揺れにも強い
- **LAN リモート検索** — 別 PC の Argos 索引も、同じネットワークから検索可能

対応形式: PDF / Word（DOCX・DOC） / 一太郎（JTD） / Excel（XLS・XLSX） / テキスト / Markdown

## ダウンロード

最新版は GitHub Releases から入手できます。

**[Releases（インストーラ）](https://github.com/aquariumyossy/argos/releases/latest)**

| ファイル | 用途 |
|----------|------|
| `Argos-Setup-v*.exe` | **推奨** — ダブルクリックでインストール（管理者権限不要） |
| `Argos-v*-windows-x64.zip` | インストーラ・ポータブル版・手順書の一式 |

動作環境: **Windows 10 / 11（64bit）**  
※ WebView2 が無い場合は、インストーラが自動で取得します。

Windows の SmartScreen で「不明な発行元」と表示されることがあります。コード署名証明書は未使用のためです。内容を確認のうえ実行してください。

## 使い方

1. トレイアイコンから「設定を開く」
2. 検索対象フォルダを追加
3. 「今すぐインデックス」を実行
4. 任意アプリで文字列を選択し `Ctrl+Alt+A`（変更可）
5. ポップアップで結果を確認 — `Enter` で開く / `Ctrl+Enter` でプレビュー / `Esc` で閉じる

詳しい操作はアプリ内の「操作方法」タブも参照してください。

## LAN リモート検索

別 PC で動いている Argos の索引を、同じ LAN 上の別の Argos から検索できます。

### ホスト側（索引がある PC）

1. 検索対象フォルダを登録しインデックスする
2. 設定の「リモート」タブで「リモート検索サーバを有効にする」をオン
3. 表示された共有トークンを控える（クライアントに同じ値を入れる）
4. Windows ファイアウォールでポート（既定 `17890`）の受信を許可する

### クライアント側

1. 「リモート」タブで検索モードを「リモートのみ」または「ハイブリッド」に設定
2. リモート URL（例: `http://192.168.x.x:17890`）とホストと同じトークンを入力
3. 「接続テスト」で確認してから保存

リモート結果のパスがホストのローカルドライブ（`C:\...` など）の場合、クライアントからはファイルを開けないことがあります。プレビューは利用可能です。両方から開けるようにするには、ホスト側でフォルダに「公開パス（UNC）」（例: `\\HostName\Share`）を設定してから再インデックスしてください。

## ライセンス・クレジット

本ソフトウェアは [Apache License 2.0](LICENSE) に基づき提供されます。

開発: 半蔵門総合法律事務所　弁護士　吉田秀平  

使用ライブラリのクレジットは、アプリ設定の「クレジット」タブを参照してください。

## 開発者向け

前提: Node.js、Rust、Visual Studio Build Tools（MSVC）

```bash
npm install
npm run tauri dev
```

初回ビルドは Lindera IPADIC 埋め込みのため時間がかかることがあります。

### 配布パッケージの作成

```bash
npm run package
```

成果物:

- `release/Argos-v*/Argos-Setup-v*.exe`
- `release/Argos-v*-windows-x64.zip`

Release 説明文のひな形: [`scripts/release-notes.template.md`](scripts/release-notes.template.md)

### 技術

- Tauri 2 + React + TypeScript
- Rust / SQLite / Tantivy / Lindera（IPADIC）
- ファイル監視: notify
- LAN 検索 API: axum（ホスト） / reqwest（クライアント）

データ保存先: `%APPDATA%\Argos\`
