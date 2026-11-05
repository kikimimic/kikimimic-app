# サインインしたときだけ Google カレンダーを読み、会議の直前に録音を勧める

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-11-06 |
| **Decision-makers** | staticWagomU |
| **Consulted** | Claude Code |
| **Informed** | - |

ADR-120000 の「既定の動作でネットワークを使わない」を保ったまま、例外を1つ足します。

## Context and Problem Statement

会議の録音は、始めるのを忘れると取り戻せません。会議の直前に「録音しますか」と聞かれれば、忘れることはほぼ無くなります。会議の予定は多くの利用者が Google カレンダーに持っています。

一方で ADR-120000 は、会議の内容を外に出さず、既定の動作でネットワークを使わないと決めています。カレンダーを読むにはネットワークと Google の OAuth が要ります。

## Decision Drivers

* 既定の動作ではネットワークを使わない（ADR-120000）
* 会議の音声と文字起こしは、これまでどおり外に出さない
* 会議中の作業を邪魔しない。キーボードを奪わない
* OSS として、clone してビルドすれば OAuth クライアントが無くても動くこと

## Considered Options

1. 利用者がサインインしたときだけ、Google Calendar API を読み取り専用で読む
2. macOS のカレンダー（EventKit）を読む
3. カレンダー連携を作らない

## Decision Outcome

**Chosen option**: 1。設計は姉妹アプリ Calendrop（`calendrop-app/Calendrop`）で実証済みの形に合わせます。

* 通信するのは、設定でサインインした後だけです。サインインしなければ、これまでどおりモデルのダウンロード以外は通信しません
* 送るのは OAuth のやり取りとカレンダーの読み取りだけです。音声、文字起こし、訳は送りません
* スコープは `calendar.calendarlist.readonly` と `calendar.events.readonly` だけです。アカウントのアドレスはメインカレンダーの ID から得るので、identity のスコープは求めません
* サインインはブラウザで行い、`127.0.0.1` の一時的なポートで受けて PKCE で交換します（OAuth 2.0 for installed apps）
* リフレッシュトークンは OS のキーチェーン（macOS は login Keychain）に置き、設定ファイルには書きません
* OAuth クライアント（種類は Desktop app）は、ビルド時の環境変数 `KKM_GOOGLE_CLIENT_ID` と `KKM_GOOGLE_CLIENT_SECRET` で埋め込みます。無しでビルドしたアプリでは、連携の設定が「このビルドでは使えません」と表示されます
* 通知は画面の中央上部に出るカードです。アプリを前面に出さず、キーボードを奪わず、全画面のアプリの上にも出ます（gpui の `WindowKind::PopUp`）。カードから録音を始められます

### Consequences

**Positive:**
* 会議の直前に、録音の開始がワンクリックになる
* 会議の URL を開くのと録音の開始を、1つのボタンで行える

**Negative:**
* README の「モデルのダウンロード以外は通信しない」に、オプトインの例外が1つ増える
* 埋め込んだクライアントの ID と secret はアプリから取り出せる。Google は Desktop app の secret を秘密として扱わず、各許可はループバックのリダイレクト、PKCE、利用者の同意で守られる
* 何人がサインインできるか、未確認アプリの警告が出るかは、メンテナーのクライアントの公開状態と確認状態で決まる
* 当面は1アカウントのメインカレンダーだけを読む

### Confirmation

* サインインしていない状態で、カレンダーのための通信が起きないこと（`calendar` のコードはサインイン後にだけ HTTP を呼ぶ）
* `KKM_CALENDAR_DEMO=1` で起動すると、架空の会議のカードが中央上部に出て、そこから録音を始められること
* クライアント無しのビルドが通り、設定に「このビルドでは使えません」と出ること

## Pros and Cons of the Options

### Google Calendar API（オプトイン）

* Good: 会議の URL（Meet、Zoom、Teams）、出欠の返答、終日の予定かどうかを、そのまま得られる
* Good: Calendrop の実装と仕様（docs/spec/cut-in.md、accounts-and-sync.md）を参考にできる
* Bad: OAuth クライアントの用意と、トークンの保管が要る

### EventKit

* Good: ネットワークもクライアントも要らない
* Bad: macOS 専用で、Windows と Linux への道が無い。カレンダー.app に Google アカウントを足してもらう必要があり、会議の URL が取れないことがある（Calendrop の ADR「read-google-calendar-through-its-api」と同じ理由）

### 作らない

* Good: 完全ローカルのまま
* Bad: 録音のし忘れが残る

## More Information

* Calendrop の ADR: `20261016-read-google-calendar-through-its-api.md`、`20261016-ship-the-google-oauth-client-inside-the-app.md`、`20261016-keep-refresh-tokens-on-the-mac-that-signed-in.md`
* OAuth 2.0 for Mobile & Desktop Apps: https://developers.google.com/identity/protocols/oauth2/native-app
