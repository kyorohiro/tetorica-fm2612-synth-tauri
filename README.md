# Tetorica FM2612 Synth — Tauri

[hello_ymfm_wasm の Synth](https://github.com/kyorohiro/hello_ymfm_wasm/tree/main/docs/synth) をデスクトップアプリとして使う Tauri 2 シェルです。
FM音色編集、鍵盤演奏、VGM / VGZ / S98 からの音色取り込み、TFI / VGI、イベント録音と looper は取り込んだ Synth の機能です。

## 起動

Node.js 22以上、Python 3、Rust と [Tauri のビルド環境](https://v2.tauri.app/start/prerequisites/) が必要です。

```sh
npm ci
npm run dev
```

`dist/` に Synth 本体を含めています。起動時のダウンロードや別の本体リポジトリは不要です。
WebView の標準ファイル選択・ドロップを使い、音色を取り込みます。音声準備後に鍵盤を押して演奏してください。
TFI / VGI と録音 JSON の書き出しはネイティブの保存ダイアログを開きます。
`Reset Offline Cache` は確認ダイアログの後、アプリの画面を再読み込みします。

## 音声出力

Vol の下の **Audio output** を開き、**Web Audio** または **Native (Rust)**
を選べます。Native はシステムの既定デバイスで開始します。別の機器を使う場合は
デバイスを選び、**Apply device** を押してください。

音源・FX・表示・録音・ルーパーは引き続き Web Audio / AudioWorklet で動き、
その最終出力をバイナリ Tauri IPC で Rust に渡します。Rust の CPAL が音声機器へ
出力します。Node sidecar、Audify、ローカル通信サーバーは使いません。
切り替え時は演奏・録音・ループ再生を停止し、録音データと確定したループは残します。
デバイスエラーが起きると演奏を止めて Web Audio に戻り、理由を表示します。

機器のサンプルレートに合わせてリサンプリングし、独立した時計のずれは緩やかに
補正します。キューは4パケットに制限し、古い音声が溜まり続けないようにしています。
WebView の音源処理とメインスレッドの IPC 転送は必要なので、Native を選んでも
UI の長い停止による欠音を完全に防ぐものではありません。

## Synth 本体を更新する

手動で用意した **Synth の release ZIP** のパスを指定します。置き場所・名前は自由で、version 指定は不要です。

```sh
npm run import:release -- ./xxx.zip
npm test
npm run dev
```

`index.html` と `synth.js` が ZIP のルートにある配布物を使います。
`dist/` と `release.lock.json` を更新し、ZIP と各ファイルの SHA-256 を記録します。
動作確認後、この2つを同じコミットに含めてください。ZIP は Git に含めません。
`dev` / `build` 前の検査は、コミットされた `dist/` と lock の一致確認だけです。

## ビルド

```sh
npm run build
```

macOS の `.app` は `src-tauri/target/release/bundle/macos/` に出ます。
アプリ名は Tetorica FM2612 Synth、識別子は `net.tetorica.fm2612-synth`。他の Tetorica アプリとは別です。

## 確認

```sh
npm test
python3 scripts/import_release.py --check
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

実機の低音量出力テスト（約400 ms）は、音声機器を使える環境で明示的に実行します。

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml native_device_smoke -- --ignored --nocapture
```

macOS の既定スピーカーで Rust 出力を確認済みです。Windows / Linux の音声機器と、
別々の機器での長時間の時計補正は未確認です。Linux のビルドには `libasound2-dev`
が必要です（CI に追加済み）。

ZIP の検査・破損時の既存 dist 保持、ネイティブ保存へのバイト列受け渡し、画面再読み込みを検証します。
macOS の実アプリで画面表示・Rust 出力への切り替えを確認済みです。バイナリ転送、
クレジット制御、リサンプリング、出力変更時の録音・ループ保持、エラー復帰にも
回帰テストがあります。既存の TFI 保存・再取り込み・再読み込みも引き続き検証対象です。
Windows / Linux の実アプリ操作、複数デバイスでの長時間演奏は別途確認してください。

## 配布

GitHub Actions の **Build Desktop** は macOS ARM64 / x86_64、Windows x86_64、Linux x86_64 / ARM64 をビルドします。
手動実行では Artifacts、`v*` タグの push では Draft Release に DMG / NSIS / DEB / AppImage を添付します。
タグを作る前に package.json / package-lock.json、Cargo.toml / Cargo.lock、tauri.conf.json のアプリ版番号を揃えてください。
署名・公証・自動更新は未対応です。CI と各 OS の動作はローカル macOS ビルドとは別に確認が必要です。

## 構成

- `dist/`：上流 ZIP の内容。Tauri 用に書き換えません。
- `desktop/desktop-interface.js`：保存・再読み込みの接続。ローカルの main 画面だけに適用。
- `desktop/audio-interface.js` / `audio-worklet.js`：出力設定と最終 PCM の捕捉。
- `src-tauri/src/audio.rs`：Rust 出力デバイス・バイナリ IPC・バッファ管理。
- `src-tauri/`：ウィンドウ・標準メニュー・保存ダイアログ・ビルド設定。
- `scripts/import_release.py`：ZIP の取り込みと整合性確認。

同梱の音源・第三者ライセンスは `dist/LICENSE` と `dist/THIRD_PARTY_LICENSES.txt` を参照してください。

Tauri シェル独自コードは BSD-3-Clause（`LICENSE`）です。
