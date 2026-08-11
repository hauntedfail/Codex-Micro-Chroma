# Codex Micro Chroma

macOSのNow Playingに表示されるアートワーク／サムネイルから代表色をローカルで抽出し、Work Louder Codex Microの外周LEDをその色で光らせるRustデーモンです。Apple Musicだけでなく、Spotifyやブラウザなど、MediaRemoteが現在の再生元として公開するアプリを同じ経路で扱います。

実行時にOpenAI/Codexのモデル、Spotify API、Apple Music API、外部サーバーは使用しません。モデルのトークン消費、APIキー、OAuth、SIPの無効化はいずれも不要です。

## 構成

```text
macOS Now Playing (Music / Spotify / browser / other players)
  -> macOS MediaRemote
  -> Apple署名済み /usr/bin/perl + mediaremote-adapter
  -> media-remote Rust crate
  -> ローカル画像デコード・代表色抽出
  -> hidapi
  -> Codex Micro v.oai.rgbcfg RPC
```

MediaRemoteはAppleの非公開Frameworkです。macOS更新によって動作が変わる可能性があり、App Store配布向けの構成ではありません。再生元がNow Playingへサムネイルを公開しない場合、そのアプリから色は取得できません。

## 必要条件

- macOS
- Now Playingへアートワークを公開する音楽・動画プレイヤー
- 接続済みCodex Micro
- Rust 1.88以降
- Xcode Command Line Tools
- macOS標準の `/usr/bin/perl`

Codex Microを開くプロセスには、macOSの「プライバシーとセキュリティ > 入力監視」の許可が必要になる場合があります。`not permitted` が出た場合は、実行に使うTerminal、または `install` が表示する常駐ワーカーのパスを入力監視へ追加してください。

## ビルド

```bash
cargo build --release
```

## 動作確認

```bash
./target/release/codex-micro-chroma probe
./target/release/codex-micro-chroma status
```

HID経路だけを試す場合は、色とeffectを指定して点灯・消灯できます。`set`と`run`の既定effectは `breath` です。

```bash
./target/release/codex-micro-chroma set --color '#33AAFF'
./target/release/codex-micro-chroma set --color '#33AAFF' --effect snake
./target/release/codex-micro-chroma off
```

## 対応effect

Codex Microで判明している次のeffectをすべて指定できます。

| CLI名 | デバイス値 |
| --- | ---: |
| `off` | 0 |
| `solid` | 1 |
| `snake` | 2 |
| `rainbow` | 3 |
| `breath` | 4 |
| `gradient` | 5 |
| `shallow-breath` | 6 |

共通パラメータは `--brightness`、`--speed`、`--magic` で、いずれも0〜1です。effectごとの見え方や有効な組み合わせはデバイス実装に依存します。

## Now Playingへ追従

```bash
./target/release/codex-micro-chroma run
```

- 再生元またはコンテンツが変わると、新しいサムネイルを再解析します。
- 白背景と透明ピクセルを除外し、支配的な色をLED向けに明るく鮮やかに補正します。
- 新しいサムネイルを待つ間は、前コンテンツの色を消灯します。
- HID設定は既定で750ms間隔に再送し、一時停止中は最後の色を維持します。
- Control-CまたはSIGTERMで停止するとLEDを消灯します。

```bash
./target/release/codex-micro-chroma run \
  --effect breath \
  --brightness 1.0 \
  --speed 0.85 \
  --magic 0.0 \
  --poll-ms 250 \
  --refresh-ms 750
```

effectの自動的な使い分けはまだ行いません。現在は起動時に選んだeffectを、取得したすべてのNow Playingサムネイル色へ一貫して適用します。

## ログイン時に自動起動

releaseバイナリ自身をユーザーのApplication Supportへコピーし、LaunchAgentを登録します。

```bash
./target/release/codex-micro-chroma install
```

ログは `~/Library/Logs/CodexMicroChroma/` に保存されます。停止・削除:

```bash
~/Library/Application\ Support/CodexMicroChroma/codex-micro-chroma uninstall
```

`uninstall` はLaunchAgentのplistとコピーした実行ファイルだけを削除し、診断用ログは残します。

## セキュリティ境界

- SIPを変更しません。
- root、`sudo`、コード注入を使用しません。
- ネットワークへ接続しません。
- 再生操作を行いません。
- HIDはCodex MicroのVID/PID/usage pageが完全一致するインターフェースを1台だけ開きます。
- プロセス間ロックで同時LED書き込みを直列化します。

## 開発時の確認

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## ライセンス

このプロジェクトはMIT Licenseです。MediaRemote Adapterおよび参照したCodex Micro HID実装の帰属は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。
