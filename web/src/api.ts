export type SessionView = {
  name: string;
  profile: string;
  image: string;
  resources: { width: number; height: number; cpus: number; memory_mib: number };
  allow_private: string[];
  state: "stopped" | "starting" | "ready" | "stopping" | "failing" | "failed";
  generation: number;
  stage: string | null;
  failure: string | null;
  machine: string | null;
  vnc_port: number | null;
  input: "interactive" | "view-only" | null;
};

export type Tab = { id: string; title: string; url: string };

export class SignedOut extends Error {}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(path, { credentials: "same-origin", ...init });
  if (response.status === 401) throw new SignedOut("Not signed in. Run `bctl serve --open`.");
  if (!response.ok) throw new Error((await response.text()) || `${response.status} ${response.statusText}`);
  return response;
}

export async function sessions(): Promise<SessionView[]> {
  const body = (await (await request("/api/sessions")).json()) as { sessions: SessionView[] };
  return body.sessions;
}

export async function tabs(session: string): Promise<Tab[]> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/tabs`)).json()) as Tab[];
}

export async function focusTab(session: string, id: string): Promise<void> {
  await request(`/api/sessions/${encodeURIComponent(session)}/tabs/${encodeURIComponent(id)}/activate`, { method: "POST" });
}

export async function stop(session: string): Promise<void> {
  await request(`/api/sessions/${encodeURIComponent(session)}/down`, { method: "POST" });
}

export function vncUrl(session: string): string {
  return `ws://${location.host}/api/sessions/${encodeURIComponent(session)}/vnc`;
}

export type Control = { authority: "automation" | "yielding" | "human"; mine: boolean; settling: number };

export const automation: Control = { authority: "automation", mine: false, settling: 0 };

export function authorityText(c: Control, runWaits: boolean): string {
  if (c.authority === "human") {
    return c.mine
      ? "You have control: your keyboard and pointer reach the browser, and native commands that change the page are refused."
      : "Another viewer has control. You are watching.";
  }
  if (c.authority === "yielding") {
    return c.mine
      ? `Yielding: waiting for ${c.settling} native ${c.settling === 1 ? "command" : "commands"} to stop before your input reaches the browser.`
      : "Another viewer is taking control. You are watching.";
  }
  return runWaits ? "Automation holds control and the run waits for you." : "Automation drives the browser.";
}

export async function control(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control`)).json()) as Control;
}

export async function takeControl(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control`, { method: "POST" })).json()) as Control;
}

export async function resumeAutomation(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control/resume`, { method: "POST" })).json()) as Control;
}

export type Attempt = {
  number: number;
  state: string;
  outcome: string | null;
  detail: string | null;
  pid: number | null;
  started_ms: number;
  ended_ms: number | null;
};

export type Run = {
  id: number;
  session: string;
  job: string | null;
  kind: "exec" | "grab";
  state: string;
  outcome: string | null;
  detail: string | null;
  profile: string;
  image: string;
  image_digest: string;
  browser_version: string;
  config: unknown;
  input_digest: string;
  output_dir: string;
  created_ms: number;
  progress: { pages_done: number; pages_queued: number; assets_done: number } | null;
  interaction: { reason: string; url: string } | null;
  attempts: Attempt[];
};

export type LogChunk = { start: number; end: number; text: string };
export type Artifacts = { files: { path: string; size: number }[]; truncated: boolean };

export async function sessionRuns(session: string): Promise<Run[]> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/runs`)).json()) as Run[];
}

export async function run(id: number): Promise<Run> {
  return (await (await request(`/api/runs/${id}`)).json()) as Run;
}

export async function runLog(id: number, offset: number | null): Promise<LogChunk> {
  const query = offset === null ? "" : `?offset=${offset}`;
  return (await (await request(`/api/runs/${id}/log${query}`)).json()) as LogChunk;
}

export async function runArtifacts(id: number): Promise<Artifacts> {
  return (await (await request(`/api/runs/${id}/artifacts`)).json()) as Artifacts;
}

export function artifactUrl(id: number, path: string): string {
  return `/api/runs/${id}/artifacts/${path.split("/").map(encodeURIComponent).join("/")}`;
}

export async function resumeRun(id: number): Promise<Run> {
  return (await (await request(`/api/runs/${id}/resume`, { method: "POST" })).json()) as Run;
}
