# Codex Micro Chroma

macOSのNow Playingに表示されるアートワーク／サムネイルから代表色をローカルで抽出し、Core Audio Process Tapから得たシステム音声の強さ・帯域・音色・立ち上がり・周期性・ステレオの広がりを解析して、Work Louder Codex Microの外周LEDを動かすRustデーモンです。Apple Musicだけでなく、Spotifyやブラウザなど、MediaRemoteが現在の再生元として公開するアプリを同じ経路で扱います。

実行時にOpenAI/Codexのモデル、Spotify API、Apple Music API、外部サーバーは使用しません。モデルのトークン消費、APIキー、OAuth、SIPの無効化はいずれも不要です。

## 構成

```text
macOS Now Playing (Music / Spotify / browser / other players)
  -> macOS MediaRemote
  -> Apple署名済み /usr/bin/perl + mediaremote-adapter
  -> media-remote Rust crate
  -> ローカル画像デコード・代表色抽出
  -> representative artwork color

macOS system output
  -> public Core Audio Process Tap
  -> local Rust DSP (loudness / bands / centroid / flatness / flux / onset /
                     stereo width / pulse / tempo confidence)
  -> LightingComposer (effect state machine / smoothing / hysteresis)
  -> persistent hidapi session
  -> Codex Micro v.oai.rgbcfg RPC
```

MediaRemoteはAppleの非公開Frameworkです。macOS更新によって動作が変わる可能性があり、App Store配布向けの構成ではありません。再生元がNow Playingへサムネイルを公開しない場合、そのアプリから色は取得できません。

## 必要条件

- macOS 14.2以降（reactive mode。`--mode static`はProcess Tap不要）
- Now Playingへアートワークを公開する音楽・動画プレイヤー
- 接続済みCodex Micro
- Rust 1.88以降
- Xcode Command Line Tools
- macOS標準の `/usr/bin/perl`

Codex Microを開くプロセスには、macOSの「プライバシーとセキュリティ > 入力監視」の許可が必要になる場合があります。`not permitted` が出た場合は、実行に使うTerminal、または `install` が表示する常駐ワーカーのパスを入力監視へ追加してください。

reactive modeの初回起動時にはmacOSの「画面収録とシステムオーディオ録音」（OSバージョンによっては「システムオーディオ録音」）許可が表示されます。音声は特徴量へ変換するだけで、録音・保存・ネットワーク送信しません。workerは許可されるまで同じプロセス内で再試行します。権限を使わない場合は `--mode static` を指定できます。

## ビルド

```bash
cargo build --release
```

## 動作確認

```bash
./target/release/codex-micro-chroma probe
./target/release/codex-micro-chroma status
./target/release/codex-micro-chroma audio-probe --seconds 15
```

HID経路だけを試す場合は、色とeffectを指定して点灯・消灯できます。`set`と`run`の既定effectは `breath` です。

```bash
./target/release/codex-micro-chroma set --color '#33AAFF'
./target/release/codex-micro-chroma set --color '#33AAFF' --effect snake
./target/release/codex-micro-chroma off
```

## 対応effect

Codex Microで判明している次のeffectをすべて指定できます。

| CLI名 | デバイス値 | reactive modeでの役割 |
| --- | ---: | --- |
| `off` | 0 | 一定時間の無音・停止 |
| `solid` | 1 | 発話や中央に定位した直接的な音 |
| `snake` | 2 | bass、pulse、周期性の強い場面 |
| `rainbow` | 3 | 高音量・高flux・強いonsetが重なるclimax |
| `breath` | 4 | 持続的・調性的で滑らかな音 |
| `gradient` | 5 | ステレオの広がりが強い音 |
| `shallow-breath` | 6 | quiet、intro、outro |

共通パラメータは `--brightness`、`--speed`、`--magic` で、いずれも0〜1です。effectごとの見え方や有効な組み合わせはデバイス実装に依存します。

## Now Playingへ追従

```bash
./target/release/codex-micro-chroma run
```

- 再生元またはコンテンツが変わると、新しいサムネイルを再解析します。
- タイトルがない再生元もサムネイルfingerprintをcontent identityとして扱い、同じタイトル内で画像だけが変化した場合も追従します。
- 白背景と透明ピクセルを除外し、支配的な色をLED向けに明るく鮮やかに補正します。
- 新しいサムネイルを待つ間は、前コンテンツの色を消灯します。
- 既定の `reactive` modeでは、相対音量をbrightness、複合的な運動性をspeed、音の広がり・変化をmagicへ反映します。
- onset、flux、pulse、bassの短いイベントはattack/release envelopeで保持し、650msのcandidate dwellを通過できるようにします。僅差の候補はhysteresisで維持するため、beatが次のaudio frameで消えても`snake`などの動的effectへ到達します。
- effectは2秒のminimum holdでちらつきを防ぎます。同じpatternが12秒を超えて最有力のままなら、意味的に近い次点へphrase-levelで譲り、`breath`など一種類への固着を防ぎます。`rainbow`には12秒のcooldownがあります。
- effectごとにspeedとmagicのprofileを変え、`solid`は静止、`snake`はbeat駆動、`gradient`はstereo width、`rainbow`はclimax、2種類のbreathは異なる深さとして送信します。短い休符では消灯しません。
- HIDは一度開いた接続を再利用し、既定100ms間隔で更新します。起動時にデバイスまたは入力監視権限が未準備ならworkerを終了せず待機し、切断・response timeoutなどのtransport errorでは一度だけ自動再接続します。
- 再生中にProcess Tapのframeが3秒停止するか、完全なゼロframeが15秒続いた場合は、30秒のcooldownを設けてTapとaggregate deviceを再生成します。
- Control-CまたはSIGTERMで停止するとLEDを消灯します。

```bash
./target/release/codex-micro-chroma run \
  --mode reactive \
  --brightness 1.0 \
  --poll-ms 250 \
  --device-ms 100
```

従来どおり一つのeffectを固定する場合:

```bash
./target/release/codex-micro-chroma run \
  --mode static \
  --effect breath \
  --brightness 1.0 \
  --speed 0.85 \
  --magic 0.0 \
  --refresh-ms 750
```

`audio-probe`はLEDへ書き込まず、Process Tapから計算した `AudioFeatureFrame` をJSON Linesで表示します。アルゴリズム調整や権限確認に使用できます。

## ログイン時に自動起動

releaseバイナリ自身をユーザーのApplication Supportへ一時コピーし、固定identifier `com.local.codex-micro-chroma` でad-hoc署名してからatomic renameし、Aquaセッション限定のLaunchAgentを登録します。

```bash
./target/release/codex-micro-chroma install
```

初回install直後にシステムオーディオ許可が表示された場合は許可してください。許可しなかった場合は、システム設定から次のworkerを「画面収録とシステムオーディオ録音」へ追加・有効化してから再度installします。

```text
~/Library/Application Support/CodexMicroChroma/codex-micro-chroma
```

ad-hoc署名のdesignated requirementはバイナリのCDHashを含むため、ソース変更後に再build・再installした場合、macOSが入力監視またはシステムオーディオ録音の再承認を求めることがあります。その場合は上記workerのスイッチを一度OFF/ONにしてください。workerは入力監視が許可されるまで同じプロセス内で待機します。Developer IDまたはローカルのCode Signing証明書で署名する配布構成では、この再承認を避けられます。

ログは `~/Library/Logs/CodexMicroChroma/` に保存されます。停止・削除:

```bash
~/Library/Application\ Support/CodexMicroChroma/codex-micro-chroma uninstall
```

`uninstall` はLaunchAgentのplistとコピーした実行ファイルだけを削除し、診断用ログは残します。

## セキュリティ境界

- SIPを変更しません。
- root、`sudo`、コード注入を使用しません。
- ネットワークへ接続しません。
- Process TapのPCMをファイルへ保存しません。
- 再生操作を行いません。
- HIDはCodex MicroのVID/PID/usage pageが完全一致するインターフェースを1台だけ開きます。
- プロセス間ロックで同時LED書き込みを直列化します。
- installed workerは固定identifierで署名しますが、Developer ID証明書は使用しません。

## 実装上の境界

`CATapDescription`はAppleのObjective-C APIであるため、Tapとaggregate deviceの生成・破棄だけを小さなObjective-C bridgeへ隔離しています。受け取ったFloat32 PCM以降のFFT、特徴抽出、相対正規化、effect score、状態遷移、HID制御はRustです。audio callbackは固定長packetをbounded channelへ `try_send` し、FFTやロックをCore Audioのreal-time thread上で実行しません。

## 開発時の確認

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## ライセンス

このプロジェクトはMIT Licenseです。MediaRemote Adapterおよび参照したCodex Micro HID実装の帰属は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。
