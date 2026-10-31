export interface ModelInfo {
  name: string;
  label: string;
  approx_mb: number;
  state: "missing" | "downloading" | "downloaded" | "loading" | "ready" | "error";
  downloaded: number;
  total: number;
  progress: number;
  error: string | null;
}

export interface Status {
  device: string;
  compute_type: string;
  default_model: string;
  models: ModelInfo[];
  max_upload_bytes: number;
}

export interface Segment {
  start: number;
  end: number;
  text: string;
}

export interface JobResult {
  language: string | null;
  duration: number | null;
  segments: Segment[];
  text: string;
  srt: string;
  vtt: string;
}

export interface JobEvent {
  id: string;
  state: "queued" | "preparing" | "transcribing" | "done" | "error";
  progress?: number;
  message?: string;
  language?: string | null;
  language_probability?: number | null;
  new_segments?: Segment[];
  segment_count?: number;
  result?: JobResult | null;
  error?: string | null;
  model_state?: ModelInfo;
}

async function errorText(res: Response): Promise<string> {
  try {
    const body = await res.json();
    if (typeof body.detail === "string") return body.detail;
  } catch {
    /* not JSON */
  }
  return `Request failed (${res.status} ${res.statusText}).`;
}

export async function getStatus(): Promise<Status> {
  const res = await fetch("/api/status");
  if (!res.ok) throw new Error(await errorText(res));
  return res.json();
}

export async function getLanguages(): Promise<Record<string, string>> {
  const res = await fetch("/api/languages");
  if (!res.ok) throw new Error(await errorText(res));
  return res.json();
}

export async function startJob(file: Blob, filename: string, language: string, model: string): Promise<string> {
  const form = new FormData();
  form.append("file", file, filename);
  form.append("language", language);
  form.append("model", model);
  let res: Response;
  try {
    res = await fetch("/api/transcribe", { method: "POST", body: form });
  } catch {
    throw new Error("The upload did not reach the server. Check that open-whisper is still running in the terminal.");
  }
  if (!res.ok) throw new Error(await errorText(res));
  return (await res.json()).job_id;
}

export async function getJob(jobId: string): Promise<JobEvent> {
  const res = await fetch(`/api/jobs/${jobId}`);
  if (!res.ok) throw new Error(await errorText(res));
  const job = await res.json();
  return { ...job, new_segments: job.new_segments ?? [] };
}

/**
 * Follow a job over SSE. If the stream breaks, fall back to polling; give up (onLost) only
 * when the server stops answering. Returns a function that stops listening.
 */
export function followJob(jobId: string, onEvent: (e: JobEvent) => void, onLost: () => void): () => void {
  let stopped = false;
  let pollTimer = 0;
  const finish = (ev: JobEvent) => ev.state === "done" || ev.state === "error";
  const es = new EventSource(`/api/jobs/${jobId}/events`);

  const poll = async (failures: number) => {
    if (stopped) return;
    try {
      const ev = await getJob(jobId);
      onEvent(ev);
      if (finish(ev)) return;
      failures = 0;
    } catch {
      if (++failures >= 5) return onLost();
    }
    pollTimer = window.setTimeout(() => poll(failures), 1000);
  };

  es.onmessage = (msg) => {
    const ev = JSON.parse(msg.data) as JobEvent;
    if (finish(ev)) {
      stopped = true;
      es.close();
    }
    onEvent(ev);
  };
  es.onerror = () => {
    if (stopped) return;
    es.close();
    void poll(0);
  };
  return () => {
    stopped = true;
    es.close();
    clearTimeout(pollTimer);
  };
}
