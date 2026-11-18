/**
 * A stand-in for the app, used when the page runs in a plain browser (`npm run dev`, the
 * screenshots, the page tests): sample meetings and settings in memory, nothing saved.
 * The real app is desktop/src-tauri; see backend.ts for what each call does there.
 */
import type { AiNotesEvent, HistoryEntry, Meeting, MeetingLive, MeetingSummary, Status } from "./api";
import type { Backend, LibraryAnswerEvent } from "./backend";

const now = Math.floor(Date.now() / 1000);
const DAY = 86_400;

function line(speaker: "me" | "them", start: number, text: string, person = 0) {
  return { speaker, start, end: start + Math.max(2, text.split(" ").length / 2.5), text, person };
}

const meetings: Meeting[] = [
  {
    id: "m-1",
    title: "Launch planning",
    started_at: now - 3600,
    duration: 12 * 60 + 40,
    state: "done",
    model: "parakeet-v3",
    system_audio: true,
    notes: "- page goes live Tuesday?\n- ask Ray for the asset list",
    lines: [
      line("me", 2, "Okay, let's go over the launch. Where are we with the page?"),
      line("them", 9, "The page is ready. I'd publish it on Tuesday after a last check of the links.", 1),
      line("them", 21, "And I can send the announcement on Wednesday morning.", 2),
      line("me", 30, "Good. I'll send Ray the asset list by Friday."),
      line("them", 41, "I'll share the release notes draft on Friday too.", 1),
      line("me", 55, "Let's leave thirty minutes for a quiet QA pass before we publish."),
    ],
    ai_notes:
      "## Launch plan\n- Publish the page **Tuesday**, after a last check of the links (00:09)\n- The announcement goes out Wednesday morning (00:21)\n- A 30-minute QA pass before publishing (00:55)\n\n## Next steps\n- **Kara** shares the release notes draft by Friday (00:41)\n- **Lincoln** sends the announcement Wednesday (00:21)\n- **Me** sends Ray the asset list by Friday (00:30)",
    ai_model: "Qwen3.5 4B",
    ai_at: now - 3000,
    chat: [],
    people: ["Kara", "Lincoln"],
    transcript_version: 0,
    ai_version: 0,
  },
  {
    id: "m-2",
    title: "Hiring sync",
    started_at: now - DAY - 7200,
    duration: 31 * 60,
    state: "done",
    model: "parakeet-v3",
    system_audio: true,
    notes: "",
    lines: [line("them", 3, "We have two strong candidates for the design role.", 1), line("me", 12, "Let's do the final interviews next week.")],
    ai_notes: "## Design role\n- Two strong candidates left (00:03)\n- Final interviews next week (00:12)",
    ai_model: "Qwen3.5 4B",
    chat: [],
    people: ["Maya"],
  },
  {
    id: "m-3",
    title: "Customer call: Acme",
    started_at: now - 3 * DAY,
    duration: 46 * 60,
    state: "done",
    model: "parakeet-v3",
    system_audio: true,
    notes: "",
    lines: [line("them", 5, "Our renewal is in March and we want the annual plan.", 1), line("me", 14, "I'll send the quote for the annual plan today.")],
    ai_notes: "",
    chat: [],
    people: [""],
  },
];

const summary = (m: Meeting): MeetingSummary => ({
  id: m.id,
  title: m.title,
  started_at: m.started_at,
  duration: m.duration,
  state: m.state,
  lines: m.lines.length,
  excerpt: (m.notes || m.lines.map((l) => l.text).join(" ")).slice(0, 160),
});

const status: Status = {
  device: "Demo (in a browser)",
  default_model: "parakeet-v3",
  models: [],
  version: "demo",
  hotkey_key: "SUPER + CTRL + X",
  hotkey_source: "app",
  meeting_key: "SUPER + CTRL + M",
  meeting_key_source: "app",
  paste_key: "SUPER + ALT + V",
  paste_key_source: "app",
  history_keep: "week",
  calls: { detect: true, auto_stop: true, available: true, apps: [] },
  recording: false,
  agents: { recording: false, program: "/usr/bin/lowkey" },
  voices: { on: true, supported: true, present: true, download: { state: "", progress: 0, error: "" }, working: [] },
  screen: { supported: false, chosen: false, check: { state: "ready", encoder: "", problem: "" } },
  export: { dir: "", auto: false, parts: ["ai_notes", "notes", "transcript"], vault: null },
  providers: { list: [], keys: [] },
  speech: { provider: "", model: "", url: "", label: null },
  ai: {
    engine: "builtin",
    provider: "custom",
    builtin_model: "qwen3.5-4b",
    url: "",
    model: "",
    has_key: false,
    auto: true,
    prompt: "",
    default_prompt: "You write meeting notes from a transcript.",
    builtin: { supported: true, downloaded: true, model: "qwen3.5-4b", download_mb: 2700, models: [{ id: "qwen3.5-4b", label: "Qwen3.5 4B", note: "", download_mb: 2700, downloaded: true }] },
  },
  options: { model: "parakeet-v3", language: "auto", idle_unload_minutes: 15, live_preview: true, preview_engine: "auto", microphone: "", vocabulary: [], theme: "system" },
  preview: { state: "ready", error: null, engine: "parakeet" },
};

const history: HistoryEntry[] = [{ id: "h-1", at: now - 600, text: "Ship the new version on Friday.", source: "key", model: "parakeet-v3", seconds: 3 }];

const noop = () => {};
const none = async () => {};

export function demoBackend(): Backend {
  const aiListeners: ((e: AiNotesEvent) => void)[] = [];
  const libraryListeners: ((e: LibraryAnswerEvent) => void)[] = [];
  const find = (id: string) => {
    const m = meetings.find((x) => x.id === id);
    if (!m) throw new Error(`unknown meeting ${id}`);
    return m;
  };
  return {
    getStatus: async () => status,
    getLanguages: async () => ({ en: "English", es: "Spanish" }),
    start: async () => "job-1",
    follow: () => noop,
    copy: none,
    save: async () => true,
    saveOptions: none,
    providerModels: async () => [],
    quit: none,
    chooseExportFolder: async () => false,
    screenForget: none,
    screenCheck: none,
    testEvent: none,
    setHotkey: async () => status,
    setMeetingHotkey: async () => status,
    setPasteHotkey: async () => status,
    microphones: async () => ({ mics: [], using: { id: null, name: "Default", note: null }, saved: "", avoid_bluetooth: true, bluetooth_aware: false, selectable: false }),
    micTest: none,
    onMicLevel: noop,
    history: { list: async () => history, remove: async () => true, clear: none, onAdded: noop },
    adoptHotkey: async () => ({ status, backup: "", start_at_login: false }),
    onStatus: noop,
    pickFile: async () => null,
    onFileDrop: noop,
    onOpenFiles: noop,
    library: {
      ask: async (question) => {
        const text = `In **Launch planning**, the page goes live on Tuesday [1], and Kara shares the release notes by Friday [2]. (Demo answer to: ${question})`;
        const sources = [
          { n: 1, meeting_id: "m-1", title: "Launch planning", started_at: meetings[0].started_at, at: 9, text: meetings[0].lines[1].text },
          { n: 2, meeting_id: "m-1", title: "Launch planning", started_at: meetings[0].started_at, at: 41, text: meetings[0].lines[4].text },
        ];
        setTimeout(() => libraryListeners.forEach((cb) => cb({ state: "done", text, sources, detail: "" })), 300);
      },
      onAnswer: (cb) => void libraryListeners.push(cb),
    },
    meetings: {
      start: async (title) => {
        const m: Meeting = { id: `m-${Date.now()}`, title: title || "Meeting", started_at: Math.floor(Date.now() / 1000), duration: 0, state: "recording", model: "demo", system_audio: true, notes: "", lines: [], chat: [] };
        meetings.unshift(m);
        return { meeting: m, state: "recording", elapsed: 0, pending: 0, notices: [] } as MeetingLive;
      },
      stop: async () => {
        const m = meetings[0];
        m.state = "done";
        return m;
      },
      live: async () => null,
      list: async (query) => {
        const words = (query ?? "").toLowerCase().split(/\s+/).filter(Boolean);
        return meetings.filter((m) => words.every((w) => JSON.stringify(m).toLowerCase().includes(w))).map(summary);
      },
      get: async (id) => structuredClone(find(id)),
      update: async (id, patch) => {
        const m = find(id);
        if (patch.title !== undefined) m.title = patch.title;
        if (patch.notes !== undefined) m.notes = patch.notes;
      },
      remove: async (id) => {
        meetings.splice(meetings.findIndex((m) => m.id === id), 1);
      },
      markdown: async (id) => `# ${find(id).title}`,
      text: async (id, part) => (part === "notes" ? find(id).ai_notes ?? "" : find(id).lines.map((l) => l.text).join("\n")),
      onLive: noop,
      onLevels: noop,
      aiNotes: async (id) => {
        setTimeout(() => aiListeners.forEach((cb) => cb({ id, state: "done", detail: "Qwen3.5 4B", progress: 1, text: find(id).ai_notes || "## Notes\n- Demo notes (00:05)" })), 300);
      },
      aiCancel: none,
      onAiNotes: (cb) => void aiListeners.push(cb),
      chat: none,
      chatClear: none,
      onAiChat: noop,
      onOpen: noop,
      editParagraph: async (id) => ({ meeting: find(id), suggestion: null }),
      nameSpeaker: async (id) => find(id),
      onVoices: noop,
      addVocabulary: none,
      videoUrl: async () => null,
      screen: none,
      saveToFolder: async () => ({ file: "", obsidian: null }),
      openInObsidian: none,
      onExport: noop,
    },
  };
}
