/* Kikimimic v4 mock data + fake engines, shared by all 30 variants.
   Everything is in-memory; nothing leaves the page. Expose a single global KKM. */
(function () {
  const mmss = (ms) => { const s = Math.floor(ms / 1000); return String(Math.floor(s / 60)).padStart(2, "0") + ":" + String(s % 60).padStart(2, "0"); };

  // Utterance text uses "|" between word units (Whisper tokens / Japanese subwords).
  const U = (id, lane, spk, start, end, text, tr, extra = {}) => ({ id, lane, spk, start, end, text: text.replaceAll("|", ""), units: text.split("|"), tr, reviewed: false, ...extra });

  const weekly = {
    id: "r1", title: "週次プロダクト定例", date: "2026-11-17 10:00", durationMs: 1_820_000, source: "meeting",
    calendar: "Google カレンダー", lanes: ["Me", "Them"],
    speakers: { S1: { name: "自分", auto: "SPEAKER_00" }, S2: { name: "Alex", auto: "SPEAKER_01" }, S3: { name: "", auto: "SPEAKER_02" } },
    utterances: [
      U("u1", "Me", "S1", 4_000, 9_200, "では|週次|定例を|始めます。|まず|リリースの|状況から|お願いします。", "Let's start the weekly sync. First, the release status, please."),
      U("u2", "Them", "S2", 10_100, 17_800, "Sure.| The| release| candidate| is| ready,| but| QA| found| two| blockers| in| the| export| flow.", "はい。リリース候補はできていますが、QA がエクスポートで2件のブロッカーを見つけました。"),
      U("u3", "Me", "S1", 18_400, 23_000, "その|2件は|今週中に|直せそうですか？", "Can those two be fixed this week?"),
      U("u4", "Them", "S2", 23_600, 31_900, "One| is| a| one-line| fix.| The| other| needs| a| decision| on| the| SRT| format.", "1件は1行の修正です。もう1件は SRT 形式について決める必要があります。"),
      U("u5", "Them", "S3", 32_400, 38_000, "I| think| we| should| keep| speaker| names| in| the| SRT.", "SRT には話者名を残すべきだと思います。"),
      U("u6", "Me", "S1", 37_200, 44_500, "賛成です。|話者名|付きで|出す|ことに|しましょう。", "I agree. Let's export with speaker names."),
      U("u7", "Them", "S2", 45_000, 51_000, "Okay.| I'll| update| the| exporter| by| Thursday.", "了解です。木曜までにエクスポーターを更新します。"),
      U("u8", "Me", "S1", 52_000, 58_400, "ありがとう|ございます。|次に|取り込み|機能の|件ですが、|誰が|見ますか？", "Thanks. Next, the import feature — who will take it?"),
      U("u9", "Them", "S3", 59_000, 64_000, "Not| sure| yet.| Let's| decide| next| week.", "まだ分かりません。来週決めましょう。"),
      U("u10", "Me", "S1", 64_800, 70_100, "分かりました。|では|担当は|未定の|まま|にします。", "Understood. We'll leave the owner undecided for now."),
    ],
    minutes: {
      summary: [{ text: "リリース候補は完成、QA がエクスポートで2件のブロッカーを報告。", ev: ["u2"] }, { text: "SRT は話者名付きで出力すると合意。", ev: ["u5", "u6"] }],
      decisions: [{ text: "SRT に話者名を含める", ev: ["u6"] }],
      actions: [{ text: "エクスポーターを更新する", who: "Alex", due: "木曜", ev: ["u7"] }],
      open: [{ text: "取り込み機能の担当者", ev: ["u8", "u9", "u10"] }],
      revision: 2, generated: "2026-11-17 10:32", model: "Qwen3 4B Instruct 2507",
    },
    stats: {
      Me: { share: 0.46, longestMs: 7_300, cpm: 312, questions: 2, overlaps: 1 },
      Them: { share: 0.54, longestMs: 8_300, cpm: 148, questions: 0, overlaps: 1 },
      headphones: true,
    },
    advisor: [{ id: "a1", kind: "未決事項", text: "「取り込み機能の担当」が決まっていません。", ev: ["u9", "u10"] }],
    diarized: true, status: "exported", note: { path: "Meetings/2026-11-17 週次プロダクト定例.md", state: "fresh" },
  };

  const recordings = [
    weekly,
    { id: "r2", title: "顧客インタビュー（対面）", date: "2026-11-14 15:30", durationMs: 2_460_000, source: "face", lanes: ["Me"],
      speakers: { S1: { name: "自分", auto: "SPEAKER_00" }, S2: { name: "田中さん", auto: "SPEAKER_01" } },
      utterances: [
        U("v1", "Me", "S1", 2_000, 6_000, "本日は|お時間|ありがとう|ございます。", "Thank you for your time today."),
        U("v2", "Me", "S2", 6_500, 13_000, "いえいえ。|議事録の|手間が|一番の|課題|なんです。", "Not at all. The biggest pain is taking minutes."),
        U("v3", "Me", "S1", 13_500, 18_000, "今は|どう|やって|残して|いますか？", "How do you record them now?"),
        U("v4", "Me", "S2", 18_500, 25_000, "手書きの|メモを|あとで|清書|しています。", "I write notes by hand and clean them up later."),
      ], minutes: null, stats: { Me: { share: 1, longestMs: 6_500, cpm: 280, questions: 1, overlaps: 0 }, headphones: false }, advisor: [], diarized: true, status: "diarized", note: null },
    { id: "r3", title: "Design review (imported)", date: "2026-11-12 18:00", durationMs: 3_120_000, source: "import", importedFrom: "design-review.m4a", lanes: ["Imported"],
      speakers: { S1: { name: "", auto: "SPEAKER_00" }, S2: { name: "", auto: "SPEAKER_01" } },
      utterances: [
        U("w1", "Imported", "S1", 1_000, 6_000, "Let's| look| at| the| new| recording| detail| view.", "新しい録音の詳細画面を見ましょう。"),
        U("w2", "Imported", "S2", 6_500, 12_000, "The| review| buttons| should| stay| in| the| same| row.", "校閲ボタンは同じ行に置いたままにすべきです。"),
      ], minutes: null, stats: null, advisor: [], diarized: false, status: "transcribed", note: null },
    { id: "r4", title: "1on1", date: "2026-11-10 13:00", durationMs: 1_200_000, source: "meeting", lanes: ["Me", "Them"],
      speakers: {}, utterances: [U("x1", "Me", null, 1_000, 4_000, "最近|どう？", "How have things been?"), U("x2", "Them", null, 4_500, 9_000, "Pretty| good,| busy| with| the| launch.", "順調です。ローンチで忙しいです。")],
      minutes: null, stats: null, advisor: [], diarized: false, status: "minutes", note: { path: "Meetings/2026-11-10 1on1.md", state: "stale" } },
  ];


  // ~90 meetings a month (the primary user's real volume). Lightweight entries, same shape.
  // status: "recorded" (offline audio, not transcribed) → "transcribed" → "diarized" → "minutes" → "exported" (note in Obsidian)
  (function addMonth() {
    const titles = ["朝会", "1on1", "週次プロダクト定例", "顧客ヒアリング", "設計レビュー", "採用面接", "営業同行（対面）", "部門定例", "振り返り", "企画ブレスト", "Vendor sync", "Partner call"];
    const base = new Date("2026-11-17T09:00:00");
    let n = 0;
    for (let day = 0; day < 30 && recordings.length < 92; day++) {
      const d = new Date(base); d.setDate(d.getDate() - day);
      if (d.getDay() === 0 || d.getDay() === 6) continue;
      const count = day === 0 ? 1 : 4 + (day % 2);
      for (let k = 0; k < count && recordings.length < 92; k++) {
        n++;
        const h = 9 + k * 2, title = titles[(day * 5 + k) % titles.length];
        const face = /対面|ヒアリング|面接/.test(title);
        const date = d.toISOString().slice(0, 10) + " " + String(h).padStart(2, "0") + ":00";
        const status = day <= 1 ? ["recorded", "transcribed", "diarized", "minutes", "exported"][(k + day) % 5] : day <= 3 ? (k % 3 ? "exported" : "minutes") : "exported";
        recordings.push({
          id: "m" + n, title, date, durationMs: (20 + ((n * 7) % 40)) * 60_000, source: face ? (n % 2 ? "face" : "import") : "meeting",
          importedFrom: face && n % 2 === 0 ? `voice-memo-${n}.m4a` : undefined, lanes: face ? ["Me"] : ["Me", "Them"], speakers: {},
          utterances: status === "recorded" ? [] : [U("m" + n + "a", "Me", null, 1_000, 5_000, "では|始めます。", "Let's begin."), U("m" + n + "b", face ? "Me" : "Them", null, 5_500, 9_000, "よろしく|お願いします。", "Nice to meet you.")],
          minutes: null, stats: null, advisor: [], diarized: status !== "recorded" && status !== "transcribed", status,
          note: status === "exported" ? { path: `Meetings/${date.slice(0, 10)} ${title}.md`, state: n % 11 === 0 ? "stale" : "fresh" } : null,
        });
      }
    }
  })();

  const folders = [
    { path: "~/Documents/notes", files: [
      { name: "release-plan.md", lines: ["# Release plan", "SRT export must keep speaker names.", "Import feature owner: TBD"] },
      { name: "interview-memo.txt", lines: ["田中さん: 議事録の手間が課題", "手書きメモを清書している"] },
    ] },
  ];

  const calendar = {
    signedIn: "you@example.com", lastRead: "10:58",
    events: [
      { id: "e0", title: "朝会", start: "09:30", inMin: -90, url: "https://meet.google.com/xyz", auto: true, done: "r1" },
      { id: "e1", title: "デザインレビュー", start: "11:30", inMin: 2, url: "https://meet.google.com/abc", auto: false },
      { id: "e1b", title: "顧客ヒアリング（対面）", start: "13:00", inMin: 92, url: null, auto: false, offline: true },
      { id: "e2", title: "1on1 with Alex", start: "14:00", inMin: 152, url: "https://zoom.us/j/1", auto: true },
      { id: "e3", title: "採用面接", start: "16:00", inMin: 272, url: null, auto: false },
    ],
  };

  const models = [
    { id: "whisper", kind: "asr", name: "Whisper large-v3-turbo (q8_0)", size: "874 MB", license: "MIT", state: "ready", inUse: true },
    { id: "haas", kind: "asr", name: "Haas ja v1.0 (Q4_K)", size: "512 MB", license: "Apache-2.0", state: "none" },
    { id: "apple", kind: "asr", name: "macOS 音声認識 (SpeechAnalyzer)", size: "—", license: "macOS", state: "builtin" },
    { id: "qwen", kind: "mt", name: "Qwen3 4B Instruct 2507 (Q4_K_M)", size: "2.5 GB", license: "Apache-2.0", state: "ready", inUse: true },
    { id: "plamo", kind: "mt", name: "PLaMo 3 Translate 2B (Q4_K_M)", size: "1.6 GB", license: "PLaMo Community", state: "license" },
    { id: "pyannote", kind: "diar", name: "Speaker diarization (pyannote)", size: "32 MB", license: "MIT", state: "ready", inUse: true },
    { id: "nemotron", kind: "diar", name: "Nemotron diarization (provisional)", size: "480 MB", license: "NVIDIA Open", state: "none" },
  ];

  const settings = {
    uiLang: "ja", spoken: ["ja", "en"], translateInto: "ja", mutual: true, asr: "whisper", mt: "qwen", diar: "pyannote",
    clearOnStart: true, saveFaceToFace: false, autoRecord: false, autoStopGraceSec: 60, advisor: "manual", webResearch: false,
    searchFolders: ["~/Documents/notes", "~/Obsidian/Work/Meetings"],
    obsidian: { vault: "~/Obsidian/Work", folder: "Meetings", fileName: "{date} {title}.md", autoExport: "minutes", frontmatter: ["date", "attendees", "duration", "source", "tags"], tags: ["meeting"], includeTranscript: true, includeTranslation: false }, recordDir: "~/Music/kikimimic",
    glossary: [["きき耳ミック", "Kikimimic"], ["ハース", "Haas"], ["ジェミニ", "Gemini"]],
  };

  const importQueue = [
    { id: "q1", file: "all-hands.mp3", size: "48 MB", durMs: 3_600_000, state: "done", progress: 1 },
    { id: "q2", file: "standup-part1.wav", size: "92 MB", durMs: 540_000, state: "running", progress: 0.42, joinGroup: "standup" },
    { id: "q3", file: "standup-part2.wav", size: "88 MB", durMs: 510_000, state: "pending", progress: 0, joinGroup: "standup" },
    { id: "q4", file: "broken.m4a", size: "3 MB", durMs: 0, state: "failed", progress: 0, error: "デコードに失敗しました (afconvert: 不明な形式)" },
    { id: "q5", file: "all-hands-copy.mp3", size: "48 MB", durMs: 3_600_000, state: "skipped", progress: 1, note: "同じ入力・設定で完了済み" },
  ];

  const evaluation = {
    recording: "r1", reference: "weekly.rttm", mode: "der", // or "disagreement"
    range: [0, 70_000], collarMs: 250, overlap: "include",
    result: { der: 0.124, miss: 0.041, fa: 0.032, confusion: 0.051, scored: 66_300 },
    mismatches: [
      { start: 37_200, end: 38_000, kind: "overlap / miss", ref: "S3", hyp: "—" },
      { start: 59_000, end: 60_400, kind: "confusion", ref: "S3", hyp: "S2" },
      { start: 45_000, end: 45_300, kind: "false alarm", ref: "—", hyp: "S2" },
    ],
    hashes: { audio: "sha256:9f2c…a1", rttm: "sha256:41be…77", transcript: "sha256:c03d…5e" },
  };

  // ---- Fake engines ----
  function unitsWithTimes(u) {
    const total = u.units.reduce((n, w) => n + w.length, 0) || 1; let t = u.start;
    return u.units.map((w) => { const s = t; t += (u.end - u.start) * (w.length / total); return { w, start: Math.round(s), end: Math.round(t) }; });
  }
  function findUtt(id) { for (const r of recordings) { const u = r.utterances.find((x) => x.id === id); if (u) return { rec: r, utt: u }; } return null; }
  function speakerName(rec, spk) { if (!spk) return null; const s = rec.speakers[spk]; return s ? (s.name || s.auto) : spk; }
  function speakerColor(spk) { return { S1: "var(--spk-1)", S2: "var(--spk-2)", S3: "var(--spk-3)", S4: "var(--spk-4)" }[spk] || "var(--muted)"; }

  function search(q) {
    q = (q || "").trim().toLowerCase(); if (!q) return [];
    const out = [];
    for (const r of recordings) {
      if (r.title.toLowerCase().includes(q) || r.date.replace(/\D/g, "").includes(q)) out.push({ type: "meeting", rec: r.id, title: r.title, date: r.date, excerpt: "タイトル・日付に一致" });
      for (const u of r.utterances) {
        for (const [field, text] of [["原文", u.text], ["訳文", u.tr || ""]]) if (text.toLowerCase().includes(q)) out.push({ type: "utterance", rec: r.id, utt: u.id, title: r.title, date: r.date, at: u.start, field, excerpt: text });
      }
    }
    for (const f of folders) for (const file of f.files) file.lines.forEach((line, i) => { if (line.toLowerCase().includes(q)) out.push({ type: "file", path: f.path + "/" + file.name, line: i + 1, excerpt: line }); });
    return out;
  }

  // Q&A: answers only with exact citations; otherwise "unconfirmed".
  function ask(recId, question) {
    const q = question.toLowerCase();
    const bank = [
      { keys: ["srt", "話者"], answer: "SRT は話者名付きで出力することに決まりました。", cites: ["u5", "u6"] },
      { keys: ["担当", "owner", "取り込み", "import"], answer: "取り込み機能の担当はまだ決まっていません。来週決める予定です。", cites: ["u9", "u10"] },
      { keys: ["ブロッカー", "blocker", "qa"], answer: "QA がエクスポートで2件のブロッカーを見つけました。1件は1行の修正、もう1件は SRT 形式の判断待ちです。", cites: ["u2", "u4"] },
      { keys: ["木曜", "thursday", "いつ"], answer: "Alex が木曜までにエクスポーターを更新します。", cites: ["u7"] },
    ];
    const hit = bank.find((b) => b.keys.some((k) => q.includes(k)));
    if (!hit || recId !== "r1") return { confirmed: false, answer: "録音の中に根拠が見つからないため、答えを確定できません。", cites: [] };
    return { confirmed: true, answer: hit.answer, cites: hit.cites.map((id) => { const f = findUtt(id); return { id, at: f.utt.start, quote: f.utt.text }; }) };
  }

  // Live session simulator: emits {type:'line'|'update'|'commit'|'translate'|'notice'|'advisor'|'question'}.
  const script = [
    ["Them", "Good morning everyone, can you hear me?", "皆さんおはようございます、聞こえますか？"],
    ["Me", "はい、聞こえています。始めましょう。", "Yes, I can hear you. Let's begin."],
    ["Them", "First item is the import feature schedule.", "最初の議題は取り込み機能のスケジュールです。"],
    ["Me", "担当は誰にしますか？", "Who should own it?"],
    ["Them", "We haven't decided that yet.", "それはまだ決めていません。"],
    ["Me", "では、来週までに決めましょう。", "Then let's decide by next week."],
  ];
  function live(mode, emit) {
    let i = 0, id = 100, stopped = false, t0 = Date.now(); const timers = [];
    const tick = () => {
      if (stopped) return;
      const [lane, text, tr] = script[i % script.length]; i++;
      if (mode === "face" && lane === "Them") { timers.push(setTimeout(tick, 200)); return; }
      const uid = "L" + (id++); const words = text.split(/(?<=\s)|(?<=[、。])/);
      emit({ type: "line", id: uid, lane, at: Date.now() - t0 });
      let k = 0;
      const grow = () => {
        if (stopped) return;
        k++; const committed = words.slice(0, Math.max(0, k - 2)).join(""), volatile = words.slice(Math.max(0, k - 2), k).join("");
        emit({ type: "update", id: uid, committed, volatile });
        if (k < words.length) timers.push(setTimeout(grow, 260));
        else {
          emit({ type: "commit", id: uid, text });
          if (mode !== "noTranslate") { emit({ type: "translating", id: uid }); timers.push(setTimeout(() => emit({ type: "translate", id: uid, text: tr }), 700)); }
          if (text.includes("haven't decided")) timers.push(setTimeout(() => emit({ type: "advisor", card: { id: "la1", kind: "未決事項", text: "「取り込み機能の担当」が未決です。", ev: [uid] } }), 900));
          timers.push(setTimeout(tick, 900));
        }
      };
      grow();
    };
    timers.push(setTimeout(tick, 400));
    timers.push(setTimeout(() => emit({ type: "question", detected: "AirPods Pro", capturing: "MacBook Pro のスピーカー" }), 9000));
    return { stop() { stopped = true; timers.forEach(clearTimeout); }, elapsed: () => Date.now() - t0 };
  }

  // Player: a clock that advances while playing; onTick(ms).
  function player(rec, onTick) {
    let pos = 0, playing = false, last = 0, raf = 0;
    const loop = (now) => { if (!playing) return; pos += now - last; last = now; if (pos >= rec.durationMs) { pos = rec.durationMs; playing = false; } onTick(pos, playing); if (playing) raf = requestAnimationFrame(loop); };
    return {
      get pos() { return pos; }, get playing() { return playing; },
      play() { if (playing) return; playing = true; last = performance.now(); raf = requestAnimationFrame(loop); onTick(pos, true); },
      pause() { playing = false; cancelAnimationFrame(raf); onTick(pos, false); },
      toggle() { playing ? this.pause() : this.play(); },
      seek(ms) { pos = Math.max(0, Math.min(rec.durationMs, ms)); onTick(pos, playing); },
    };
  }

  // Exports (Markdown / SRT / words.srt) using saved names.
  function srtTime(ms) { const h = Math.floor(ms / 3.6e6), m = Math.floor(ms / 6e4) % 60, s = Math.floor(ms / 1000) % 60, x = ms % 1000; return [h, m, s].map((n) => String(n).padStart(2, "0")).join(":") + "," + String(x).padStart(3, "0"); }
  function exportText(rec, kind) {
    if (kind === "md") return "# " + rec.title + "\n\n" + rec.utterances.map((u) => `**${mmss(u.start)} ${speakerName(rec, u.spk) || u.lane}**: ${u.text}` + (u.tr ? `\n> ${u.tr}` : "")).join("\n\n");
    if (kind === "srt") return rec.utterances.map((u, i) => `${i + 1}\n${srtTime(u.start)} --> ${srtTime(u.end)}\n${speakerName(rec, u.spk) ? "[" + speakerName(rec, u.spk) + "] " : ""}${u.text}`).join("\n\n");
    if (kind === "obsidian") {
      const ob = settings.obsidian, names = Object.values(rec.speakers).map((x) => x.name).filter(Boolean);
      const fm = ["---", `date: ${rec.date}`, `attendees: [${names.map((x) => `"[[${x}]]"`).join(", ")}]`, `duration: ${mmss(rec.durationMs)}`, `source: ${rec.source}`, `tags: [${ob.tags.join(", ")}]`, "kikimimic: " + rec.id, "---"].join("\n");
      const m = rec.minutes, sec = (h, xs) => xs && xs.length ? `\n## ${h}\n` + xs.map((x) => `- ${x.text}${x.who ? `（${x.who}・${x.due}）` : ""}`).join("\n") : "";
      const body = m ? sec("要約", m.summary) + sec("決定事項", m.decisions) + sec("宿題", m.actions) + sec("未決事項", m.open) : "\n（議事録はまだありません）";
      const tr = ob.includeTranscript ? "\n\n## 文字起こし\n" + rec.utterances.map((u) => `- ${mmss(u.start)} **${speakerName(rec, u.spk) || u.lane}**: ${u.text}` + (ob.includeTranslation && u.tr ? `\n  - ${u.tr}` : "")).join("\n") : "";
      return fm + "\n\n# " + rec.title + "\n" + body + tr + "\n";
    }
    let n = 0; return rec.utterances.flatMap((u) => unitsWithTimes(u).map((w) => `${++n}\n${srtTime(w.start)} --> ${srtTime(w.end)}\n${w.w.trim()}`)).join("\n\n");
  }

  const STATUS = { recorded: "未処理", transcribed: "文字起こし済み", diarized: "話者済み", minutes: "議事録済み", exported: "Obsidian 済み" };
  window.KKM = { STATUS, mmss, recordings, folders, calendar, models, settings, importQueue, evaluation, unitsWithTimes, findUtt, speakerName, speakerColor, search, ask, live, player, exportText,
    langs: { ja: "日本語", en: "English", zh: "中文", ko: "한국어", es: "Español", fr: "Français", de: "Deutsch" } };
})();
